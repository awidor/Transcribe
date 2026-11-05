//! Everything the windows show, decided once: the session, history, settings
//! and updates. The windows only draw it and send commands.
use crate::{
    audio::{self, AudioReceiver, NoSpeech},
    azure::{self, Words},
    credentials, hotkey, insertion,
    provider::{CleanupConfig, CleanupEngine, CleanupModel, OpenRouter, TranscriptionModel},
    rules::busy,
    s1,
    storage::{self, Entry, Settings, Store},
    update::Updater,
};
use anyhow::{anyhow, bail, Context, Result};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard, Weak,
    },
    time::Duration,
};
use tokio::{runtime::Handle, sync::watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Starting,
    Recording,
    Transcribing,
    Cleaning,
    Inserting,
    Done,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Session {
    pub phase: Phase,
    /// Milliseconds since the epoch.
    pub started_at: Option<i64>,
    pub error: Option<String>,
    pub retrying: bool,
    /// The saved transcript the widget offers after paste fails.
    pub transcript: Option<String>,
    pub words: Words,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UpdateView {
    /// "unsupported", "idle", "checking", "current", "available", "installing" or "error".
    pub phase: &'static str,
    pub current: String,
    pub version: Option<String>,
    pub error: Option<String>,
}

pub struct Bootstrap {
    pub entries: Vec<Entry>,
    pub settings: Settings,
    pub microphones: Vec<String>,
    pub has_azure_key: bool,
    pub has_open_router_key: bool,
    pub session: Session,
}

pub struct CaptureSession {
    pub token: u64,
    pub platform: &'static str,
}

/// What the app tells the windows. Sent from background threads, some while
/// the app holds its own locks: never call back into the app from the
/// listener; hand the events to the UI thread instead.
#[derive(Clone, Debug)]
pub enum Event {
    Session(Session),
    History,
    /// Show the main window on History.
    OpenHistory,
    S1(s1::Status),
    /// Capture and Cancelled events for the shortcut recorder.
    Capture(hotkey::Event),
    Update(UpdateView),
    /// Re-evaluate the widget with `widget_to_show(false)` on the UI thread.
    ShowWidget,
    HideWidget,
}

pub struct Listener {
    pub event: Box<dyn Fn(Event) + Send + Sync>,
    /// About 30 times a second while recording.
    pub level: Box<dyn Fn(f32) + Send + Sync>,
    /// Whether the main window is in front.
    pub main_focused: Arc<AtomicBool>,
}

#[derive(Default)]
struct SessionState {
    view: Session,
    id: String,
    recorder: Option<audio::Recorder>,
    automatic: bool,
    widget_requested: bool,
    cancel: CancellationToken,
}

struct State {
    listener: Listener,
    runtime: Handle,
    store: Mutex<Store>,
    session: Mutex<SessionState>,
    openrouter: OpenRouter,
    /// The region found for the saved Azure key, with the key it belongs to.
    azure_region: Mutex<Option<(String, String)>>,
    api_key: PathBuf,
    hotkeys: Arc<hotkey::Service>,
    settings_lock: tokio::sync::Mutex<()>,
    s1: Arc<s1::Engine>,
    update: Mutex<UpdateView>,
    updater: Option<Arc<Updater>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

fn unload(settings: &Settings) -> Option<Duration> {
    settings.cleanup_unload_seconds.map(Duration::from_secs)
}

fn voxtral(settings: &Settings) -> bool {
    !settings.streaming && settings.transcription_model == TranscriptionModel::Voxtral
}

fn is_no_speech(error: &anyhow::Error) -> bool {
    error.downcast_ref::<NoSpeech>().is_some()
}

impl State {
    fn emit(&self, event: Event) {
        (self.listener.event)(event)
    }

    fn settings(&self) -> Settings {
        lock(&self.store).settings()
    }

    fn history(&self) {
        self.emit(Event::History);
    }

    /// Called with the session locked.
    fn publish(&self, s: &SessionState) {
        let phase = s.view.phase;
        self.hotkeys.dismissible(
            s.widget_requested
                && matches!(
                    phase,
                    Phase::Starting | Phase::Recording | Phase::Transcribing | Phase::Cleaning
                ),
        );
        self.emit(Event::Session(s.view.clone()));
    }

    fn show_widget(&self) {
        self.emit(Event::ShowWidget);
    }

    /// Hides the widget once the pill has played its closing animation,
    /// unless a new session has opened it again in the meantime.
    fn close_widget(self: &Arc<Self>) {
        let state = self.clone();
        self.runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(260)).await;
            if lock(&state.session).view.phase != Phase::Idle {
                return;
            }
            state.emit(Event::HideWidget);
        });
    }

    fn error(&self, id: &str, message: &str) {
        let mut s = lock(&self.session);
        if s.id != id || s.cancel.is_cancelled() {
            return;
        }
        if s.recorder.take().is_some() {
            audio::chime(false);
        }
        s.view.phase = Phase::Error;
        s.view.error = Some(message.into());
        self.publish(&s);
    }

    /// Ends a session that finished with nothing to show, such as silence.
    fn dismiss(self: &Arc<Self>, id: &str) {
        {
            let mut s = lock(&self.session);
            if s.id != id {
                return;
            }
            *s = SessionState::default();
            self.publish(&s);
        }
        self.close_widget();
    }

    fn azure_key_path(&self) -> PathBuf {
        self.api_key.with_file_name("azure-api-key")
    }

    fn azure_key(&self) -> Result<String> {
        credentials::read(&self.azure_key_path()).map_err(|_| anyhow!("Azure API key required"))
    }

    fn open_router_key(&self) -> Result<String> {
        credentials::read(&self.api_key).map_err(|_| anyhow!("OpenRouter API key required"))
    }

    /// The key of the service that transcribes with these settings.
    fn transcription_key(&self, settings: &Settings) -> Result<String> {
        if voxtral(settings) {
            self.open_router_key()
        } else {
            self.azure_key()
        }
    }

    async fn region_for(&self, key: &str) -> Result<String> {
        if let Some((saved, region)) = lock(&self.azure_region).as_ref() {
            if saved == key {
                return Ok(region.clone());
            }
        }
        let region = azure::region(key).await?;
        *lock(&self.azure_region) = Some((key.into(), region.clone()));
        Ok(region)
    }

    fn cleanup_config(&self, settings: &Settings) -> Result<Option<CleanupConfig>> {
        Ok(match settings.cleanup_engine {
            CleanupEngine::Off | CleanupEngine::MaiTranscribe => None,
            CleanupEngine::S1Mini => Some(CleanupConfig::Local {
                engine: self.s1.clone(),
                styling: settings.cleanup_styling,
            }),
            CleanupEngine::OpenRouter => Some(CleanupConfig::OpenRouter {
                model: settings.cleanup_model.clone(),
                reasoning_effort: settings.cleanup_reasoning_effort,
                key: self.open_router_key()?,
            }),
        })
    }

    fn register_shortcut(&self, shortcut: &str) -> Result<()> {
        let binding = self.hotkeys.validate(shortcut)?;
        self.hotkeys.start()?;
        self.hotkeys.configure(binding);
        Ok(())
    }

    async fn toggle(self: &Arc<Self>, automatic: bool) -> Result<()> {
        let id = {
            let mut s = lock(&self.session);
            if s.view.phase == Phase::Recording {
                drop(s);
                self.stop_recording(None);
                return Ok(());
            }
            if busy(s.view.phase) {
                return Ok(());
            }
            *s = SessionState {
                id: uuid::Uuid::new_v4().to_string(),
                widget_requested: true,
                ..SessionState::default()
            };
            s.view.phase = Phase::Starting;
            self.publish(&s);
            s.id.clone()
        };
        // A shortcut started in our own window has the same save-only intent
        // as the Record button. External shortcuts still resolve their
        // destination at delivery; no application or field is captured here.
        let automatic = automatic && !self.listener.main_focused.load(Ordering::Relaxed);
        self.show_widget();
        if automatic {
            self.runtime.spawn(insertion::prepare());
        }
        let result = self.start_recording(&id, automatic).await;
        if let Err(error) = &result {
            self.error(&id, &error.to_string());
            self.show_widget();
        }
        result
    }

    async fn start_recording(self: &Arc<Self>, id: &str, automatic: bool) -> Result<()> {
        let settings = self.settings();
        let key = self.transcription_key(&settings)?;
        // A missing cleanup key fails now, not after the user has spoken.
        self.cleanup_config(&settings)?;
        if settings.cleanup_engine == CleanupEngine::S1Mini {
            self.s1.check()?;
            // The model loads while the user is still speaking.
            let engine = self.s1.clone();
            self.runtime.spawn(async move { engine.warm().await });
        }
        let weak = Arc::downgrade(self);
        let microphone = settings.microphone.clone();
        let (recorder, audio) = tokio::task::spawn_blocking(move || {
            audio::Recorder::start(
                microphone,
                Arc::new(move |level| {
                    if let Some(state) = weak.upgrade() {
                        (state.listener.level)(level);
                    }
                }),
            )
        })
        .await??;
        let cancel = {
            let mut s = lock(&self.session);
            if s.id != id || s.cancel.is_cancelled() {
                // Dropping the recorder closes the audio.
                return Ok(());
            }
            s.recorder = Some(recorder);
            s.automatic = automatic;
            s.view.phase = Phase::Recording;
            s.view.started_at = Some(now_ms());
            self.publish(&s);
            s.cancel.clone()
        };
        audio::chime(true);
        self.show_widget();
        let state = self.clone();
        let job = (id.to_owned(), cancel.clone());
        self.runtime.spawn(async move {
            state
                .transcribe(job.0, key, settings, audio, automatic, job.1)
                .await
        });
        let state = self.clone();
        let id = id.to_owned();
        self.runtime.spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => (),
                _ = tokio::time::sleep(Duration::from_secs(audio::MAX_SECONDS)) => {
                    state.stop_recording(Some(&id));
                }
            }
        });
        Ok(())
    }

    fn stop_recording(&self, expected: Option<&str>) {
        {
            let mut s = lock(&self.session);
            if s.view.phase != Phase::Recording || expected.is_some_and(|id| id != s.id) {
                return;
            }
            // Ending capture closes the audio stream; Azure then finalizes the transcript.
            s.recorder = None;
            s.view.phase = Phase::Transcribing;
            self.publish(&s);
        }
        audio::chime(false);
    }

    fn cancel(self: &Arc<Self>) {
        {
            let mut s = lock(&self.session);
            if s.view.phase == Phase::Inserting {
                return;
            }
            s.cancel.cancel();
            if s.recorder.take().is_some() {
                audio::chime(false);
            }
            *s = SessionState::default();
            self.publish(&s);
        }
        self.close_widget();
    }

    /// Whether `id` is still the live, uncancelled session.
    fn live(&self, id: &str) -> Option<MutexGuard<'_, SessionState>> {
        let s = lock(&self.session);
        (s.id == id && !s.cancel.is_cancelled()).then_some(s)
    }

    /// Sends a recording or an import to Azure, showing words as they arrive
    /// when streaming, or to Voxtral, then hands the transcript on for
    /// cleanup and delivery.
    async fn transcribe(
        self: Arc<Self>,
        id: String,
        key: String,
        settings: Settings,
        audio: AudioReceiver,
        automatic: bool,
        cancel: CancellationToken,
    ) {
        let (words, mut changes) = watch::channel(Words::default());
        let state = self.clone();
        let watched = id.clone();
        let forward = self.runtime.spawn(async move {
            while changes.changed().await.is_ok() {
                let next = changes.borrow_and_update().clone();
                let appeared = {
                    let Some(mut s) = state.live(&watched) else {
                        continue;
                    };
                    // The widget grows to show words, and only resizes when they appear.
                    let appeared = s.view.words.is_empty() && !next.is_empty();
                    s.view.words = next;
                    state.publish(&s);
                    appeared
                };
                if appeared {
                    state.show_widget();
                }
            }
        });
        let retrying = |phase: Phase| {
            let state = self.clone();
            let id = id.clone();
            move || {
                if let Some(mut s) = state.live(&id) {
                    if s.view.phase == phase && !s.view.retrying {
                        s.view.retrying = true;
                        state.publish(&s);
                    }
                }
            }
        };
        let result = if voxtral(&settings) {
            self.openrouter
                .transcribe(audio, &key, &retrying(Phase::Transcribing), cancel.clone())
                .await
        } else {
            async {
                // The microphone is already buffering while the region is looked up.
                let region = tokio::select! {
                    _ = cancel.cancelled() => bail!("Cancelled"),
                    region = self.region_for(&key) => region?,
                };
                let config = azure::Config { region, key };
                // Azure's clean style needs the whole recording.
                let clean = settings.cleanup_engine == CleanupEngine::MaiTranscribe;
                if settings.streaming && !clean {
                    azure::Stream::connect(&config, &cancel)
                        .await?
                        .run(audio, &words, &cancel)
                        .await
                } else {
                    azure::transcribe(&config, audio, clean, &cancel).await
                }
            }
            .await
        };
        let confirmed = words.borrow().text.trim().to_owned();
        drop(words);
        let _ = forward.await;
        match result {
            Ok((text, seconds)) => {
                self.process(id, text, seconds, automatic, cancel).await;
            }
            Err(_) if cancel.is_cancelled() => (),
            Err(error) if is_no_speech(&error) => self.dismiss(&id),
            Err(error) => {
                let message = error.to_string();
                // Words Azure already confirmed are kept with the failure.
                let _ = lock(&self.store).insert(&Entry {
                    id: id.clone(),
                    created_at: now_ms(),
                    text: confirmed,
                    seconds: 0.,
                    status: "failed".into(),
                    error: Some(message.clone()),
                });
                self.history();
                self.error(&id, &message);
                self.show_widget();
            }
        }
    }

    async fn process(
        self: &Arc<Self>,
        id: String,
        text: String,
        seconds: f64,
        automatic: bool,
        cancel: CancellationToken,
    ) {
        let operation = async {
            anyhow::ensure!(!cancel.is_cancelled(), "Cancelled");
            // The raw transcript is saved first, so a failed cleanup never loses it.
            lock(&self.store).insert(&Entry {
                id: id.clone(),
                created_at: now_ms(),
                text: text.clone(),
                seconds,
                status: "transcribing".into(),
                error: None,
            })?;
            self.history();
            let settings = self.settings();
            let mut text = text;
            if let Some(cleanup) = self.cleanup_config(&settings)? {
                if let Some(mut s) = self.live(&id) {
                    s.view.phase = Phase::Cleaning;
                    s.view.retrying = false;
                    self.publish(&s);
                }
                let state = self.clone();
                let retry_id = id.clone();
                let retrying = move || {
                    if let Some(mut s) = state.live(&retry_id) {
                        if s.view.phase == Phase::Cleaning && !s.view.retrying {
                            s.view.retrying = true;
                            state.publish(&s);
                        }
                    }
                };
                match self
                    .openrouter
                    .clean(&text, &cleanup, &retrying, cancel.clone())
                    .await
                {
                    Ok(cleaned) => text = cleaned,
                    Err(error) if is_no_speech(&error) => {
                        // Filler alone is silence, and leaves nothing to keep.
                        lock(&self.store).delete(&id)?;
                        self.history();
                        self.dismiss(&id);
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                }
            }
            {
                let store = lock(&self.store);
                store.edit(&id, &text)?;
                store.status(&id, "pending", None)?;
            }
            self.history();
            // Persist first: insertion failures must never lose a successful transcript.
            {
                anyhow::ensure!(!cancel.is_cancelled(), "Cancelled");
                let mut s = lock(&self.session);
                if s.id != id {
                    drop(s);
                    lock(&self.store).status(&id, "saved", None)?;
                    self.history();
                    return Ok(());
                }
                s.view.phase = Phase::Inserting;
                s.view.retrying = false;
                self.publish(&s);
            }
            let insertion_error = if automatic {
                let _pause = self.hotkeys.pause();
                insertion::insert(&id, &text)
                    .await
                    .err()
                    .map(|error| error.to_string())
            } else {
                None
            };
            let status = if insertion_error.is_none() && automatic {
                "unverified"
            } else {
                "saved"
            };
            lock(&self.store).status(&id, status, insertion_error.as_deref())?;
            self.history();
            let unpasted = {
                let mut s = lock(&self.session);
                if s.id == id {
                    s.view.phase = if insertion_error.is_some() {
                        Phase::Error
                    } else {
                        Phase::Done
                    };
                    s.view.transcript = insertion_error.as_ref().map(|_| text.clone());
                    s.view.error = insertion_error;
                    self.publish(&s);
                    s.view.transcript.is_some()
                } else {
                    false
                }
            };
            // The widget grows into the transcript card.
            if unpasted {
                self.show_widget();
            }
            Ok(())
        };
        if let Err(error) = operation.await {
            if cancel.is_cancelled() {
                // A cancelled transcription leaves nothing behind.
                let _ = lock(&self.store).delete(&id);
                self.history();
            } else {
                let message = error.to_string();
                let _ = lock(&self.store).status(&id, "failed", Some(&message));
                self.history();
                self.error(&id, &message);
            }
        }
        // Long enough for the pill's check to land before it closes.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let closing = {
            let mut s = lock(&self.session);
            let done = s.id == id && s.view.phase == Phase::Done;
            if done {
                *s = SessionState::default();
                self.publish(&s);
            }
            done
        };
        if closing {
            self.close_widget();
        }
    }

    fn change_update(&self, change: impl FnOnce(&mut UpdateView)) -> UpdateView {
        let mut update = lock(&self.update);
        change(&mut update);
        self.emit(Event::Update(update.clone()));
        update.clone()
    }

    async fn check_update(&self, manual: bool) -> Result<UpdateView> {
        let updater = self.updater.clone().context("Updates unavailable")?;
        let previous = {
            let mut update = lock(&self.update);
            if matches!(update.phase, "checking" | "installing") {
                return Ok(update.clone());
            }
            let previous = update.clone();
            update.phase = "checking";
            update.error = None;
            self.emit(Event::Update(update.clone()));
            previous
        };
        let result = tokio::task::spawn_blocking(move || updater.check())
            .await
            .map_err(anyhow::Error::from)
            .and_then(|result| result);
        Ok(self.change_update(|view| match result {
            // Background checks stay quiet: offline machines and releases
            // without an update feed are not actionable.
            Err(_) if !manual => *view = previous,
            Err(error) => {
                view.phase = "error";
                view.error = Some(error.to_string());
            }
            Ok(Some(version)) => {
                view.phase = "available";
                view.version = Some(version);
            }
            Ok(None) => {
                view.phase = "current";
                view.version = None;
            }
        }))
    }
}

/// The app: cheap to clone, and every command runs on its tokio runtime.
#[derive(Clone)]
pub struct App {
    state: Arc<State>,
}

impl App {
    /// Opens the data in `data` and registers the saved shortcut.
    pub fn new(
        data: &Path,
        listener: Listener,
        updater: Option<Updater>,
        runtime: Handle,
    ) -> Result<Self> {
        std::fs::create_dir_all(data)?;
        let _ = storage::remove_legacy_audio(data);
        let store = Store::open(&data.join("history.sqlite"))?;
        let settings = store.settings();
        let s1 = s1::Engine::new(data.join("s1"));
        {
            let _guard = runtime.enter();
            s1.set_unload(unload(&settings));
        }
        let state = Arc::new_cyclic(|weak: &Weak<State>| {
            let handler = weak.clone();
            let events = runtime.clone();
            State {
                listener,
                runtime: runtime.clone(),
                store: Mutex::new(store),
                session: Mutex::default(),
                openrouter: OpenRouter::default(),
                azure_region: Mutex::new(None),
                api_key: data.join("api-key"),
                hotkeys: hotkey::Service::new(move |event| {
                    let Some(state) = handler.upgrade() else {
                        return;
                    };
                    match event {
                        hotkey::Event::Activate => {
                            events.spawn(async move { state.toggle(true).await });
                        }
                        hotkey::Event::Dismiss => {
                            events.spawn(async move { state.cancel() });
                        }
                        hotkey::Event::Error { message } => {
                            let mut s = lock(&state.session);
                            s.view.error = Some(message);
                            state.publish(&s);
                        }
                        capture => state.emit(Event::Capture(capture)),
                    }
                }),
                settings_lock: tokio::sync::Mutex::new(()),
                s1: s1.clone(),
                update: Mutex::new(UpdateView {
                    phase: if updater.is_some() {
                        "idle"
                    } else {
                        "unsupported"
                    },
                    current: env!("CARGO_PKG_VERSION").into(),
                    version: None,
                    error: None,
                }),
                updater: updater.map(Arc::new),
            }
        });
        let mut status = s1.subscribe();
        let weak = Arc::downgrade(&state);
        runtime.spawn(async move {
            while status.changed().await.is_ok() {
                let current = status.borrow_and_update().clone();
                match weak.upgrade() {
                    Some(state) => state.emit(Event::S1(current)),
                    None => return,
                }
            }
        });
        let registering = state.clone();
        runtime.spawn(async move {
            let _guard = registering.settings_lock.lock().await;
            let shortcut = registering.settings().shortcut;
            if let Err(error) = registering.register_shortcut(&shortcut) {
                let mut s = lock(&registering.session);
                s.view.error = Some(error.to_string());
                registering.publish(&s);
            }
        });
        if cfg!(not(debug_assertions)) && state.updater.is_some() {
            let weak = Arc::downgrade(&state);
            runtime.spawn(async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                loop {
                    let Some(state) = weak.upgrade() else {
                        return;
                    };
                    let _ = state.check_update(false).await;
                    drop(state);
                    tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
                }
            });
        }
        Ok(Self { state })
    }

    pub fn bootstrap(&self) -> Bootstrap {
        let (entries, mut settings) = {
            let store = lock(&self.state.store);
            (store.list().unwrap_or_default(), store.settings())
        };
        if hotkey::platform() == "linux-evdev" {
            settings.shortcut_label = None;
        }
        let has = |path: &Path| credentials::read(path).is_ok();
        Bootstrap {
            entries,
            settings,
            microphones: audio::devices().unwrap_or_default(),
            has_azure_key: has(&self.state.azure_key_path()),
            has_open_router_key: has(&self.state.api_key),
            session: lock(&self.state.session).view.clone(),
        }
    }

    pub fn history(&self) -> Result<Vec<Entry>> {
        lock(&self.state.store).list()
    }

    pub async fn copy_entry(&self, id: &str) -> Result<()> {
        let text = lock(&self.state.store)
            .list()?
            .into_iter()
            .find(|entry| entry.id == id)
            .context("Transcript unavailable")?
            .text;
        insertion::copy(text).await
    }

    pub async fn copy_text(&self, text: String) -> Result<()> {
        insertion::copy(text).await
    }

    pub fn edit_entry(&self, id: &str, text: &str) -> Result<()> {
        anyhow::ensure!(text.len() <= 4 * 1024 * 1024, "Transcript too large");
        lock(&self.state.store).edit(id, text)
    }

    pub fn delete_entry(&self, id: &str) -> Result<()> {
        {
            let s = lock(&self.state.session);
            anyhow::ensure!(
                !(s.id == id && busy(s.view.phase)),
                "Transcription in progress"
            );
        }
        lock(&self.state.store).delete(id)
    }

    /// `key` and `azure_key` replace the saved keys when present.
    pub async fn save_settings(
        &self,
        mut settings: Settings,
        key: Option<String>,
        azure_key: Option<String>,
    ) -> Result<Settings> {
        let state = &self.state;
        settings.cleanup_model = settings.cleanup_model.trim().to_owned();
        if settings.cleanup_model.is_empty() {
            anyhow::ensure!(
                settings.cleanup_engine != CleanupEngine::OpenRouter,
                "Cleanup model required"
            );
            settings.cleanup_model = crate::provider::DEFAULT_CLEANUP_MODEL.into();
        }
        let _guard = state.settings_lock.lock().await;
        let old = state.settings();
        state.hotkeys.validate(&settings.shortcut)?;
        // A key Azure does not accept is never saved.
        if let Some(azure_key) = &azure_key {
            let trimmed = azure_key.trim().to_owned();
            let region = azure::region(&trimmed).await?;
            *lock(&state.azure_region) = Some((trimmed, region));
        }
        if let Some(key) = &key {
            credentials::save(&state.api_key, key)?;
        }
        if let Some(azure_key) = &azure_key {
            credentials::save(&state.azure_key_path(), azure_key)?;
        }
        state.register_shortcut(&settings.shortcut)?;
        settings.shortcut_label = None;
        if let Err(error) = lock(&state.store).save_settings(&settings) {
            if old.shortcut != settings.shortcut {
                if let Err(rollback) = state.register_shortcut(&old.shortcut) {
                    bail!("{error}; shortcut restore failed: {rollback}");
                }
            }
            return Err(error);
        }
        state.s1.set_unload(unload(&settings));
        if settings.cleanup_engine != CleanupEngine::S1Mini {
            state.s1.stop();
        }
        Ok(settings)
    }

    pub async fn cleanup_models(&self) -> Result<Vec<CleanupModel>> {
        self.state.openrouter.cleanup_models().await
    }

    pub fn s1_status(&self) -> s1::Status {
        self.state.s1.status()
    }

    /// Progress and failures arrive as S1 events.
    pub fn download_s1(&self, part: s1::Part) {
        let engine = self.state.s1.clone();
        self.state
            .runtime
            .spawn(async move { engine.download(part).await });
    }

    pub fn cancel_s1_download(&self, part: s1::Part) {
        self.state.s1.cancel_download(part);
    }

    /// Windows check that Settings has focus before and after beginning a capture.
    pub fn begin_shortcut_capture(&self) -> Result<CaptureSession> {
        anyhow::ensure!(
            !busy(lock(&self.state.session).view.phase),
            "Stop recording before changing the shortcut"
        );
        let token = self.state.hotkeys.begin_capture()?;
        let service = self.state.hotkeys.clone();
        self.state.runtime.spawn(async move {
            tokio::time::sleep(Duration::from_secs(15)).await;
            service.cancel_capture(Some(token));
        });
        Ok(CaptureSession {
            token,
            platform: hotkey::platform(),
        })
    }

    pub fn capture_shortcut_key(
        &self,
        token: u64,
        key: hotkey::Key,
        down: bool,
    ) -> Result<Vec<hotkey::Event>> {
        self.state.hotkeys.capture_key(token, key, down)
    }

    pub fn window_shortcut_key(&self, key: hotkey::Key, down: bool) -> Result<()> {
        self.state.hotkeys.window_key(key, down)
    }

    pub fn shortcut_binding(&self) -> Vec<Vec<u32>> {
        self.state.hotkeys.binding()
    }

    pub fn end_shortcut_capture(&self, token: Option<u64>) {
        self.state.hotkeys.cancel_capture(token);
    }

    pub fn open_history(&self) {
        lock(&self.state.session).widget_requested = false;
        self.state.emit(Event::OpenHistory);
        self.state.emit(Event::HideWidget);
    }

    /// `automatic` sessions paste where the user is typing; others only save.
    pub async fn toggle(&self, automatic: bool) -> Result<()> {
        self.state.toggle(automatic).await
    }

    pub fn cancel(&self) {
        self.state.cancel();
    }

    pub fn import_audio(&self, path: &Path) -> Result<()> {
        let state = &self.state;
        let format = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        anyhow::ensure!(
            matches!(format.as_str(), "wav" | "mp3" | "flac"),
            "Audio format unsupported"
        );
        let settings = state.settings();
        let key = state.transcription_key(&settings)?;
        state.cleanup_config(&settings)?;
        let audio = {
            let _guard = state.runtime.enter();
            audio::decode(path)?
        };
        let (id, cancel) = {
            let mut s = lock(&state.session);
            anyhow::ensure!(!busy(s.view.phase), "Recording in progress");
            *s = SessionState {
                id: uuid::Uuid::new_v4().to_string(),
                ..SessionState::default()
            };
            s.view.phase = Phase::Transcribing;
            state.publish(&s);
            (s.id.clone(), s.cancel.clone())
        };
        let task = state.clone();
        state.runtime.spawn(async move {
            task.transcribe(id, key, settings, audio, false, cancel)
                .await
        });
        Ok(())
    }

    /// Called on the UI thread. The session the widget should show now, or
    /// nothing to leave it. `active_only` re-shows a live recording that
    /// something else hid.
    pub fn widget_to_show(&self, active_only: bool) -> Option<Session> {
        // Recheck on the UI thread so a queued show cannot undo cancellation.
        let s = lock(&self.state.session);
        if !s.widget_requested {
            return None;
        }
        if !busy(s.view.phase)
            && (active_only || !matches!(s.view.phase, Phase::Error | Phase::Done))
        {
            return None;
        }
        Some(s.view.clone())
    }

    /// After showing the widget: the failure, or nothing.
    pub fn widget_shown(&self, error: Option<String>) {
        const PREFIX: &str = "Recording widget: ";
        let mut s = lock(&self.state.session);
        match error {
            Some(error) => {
                let message = format!("{PREFIX}{error}");
                eprintln!("{message}");
                if s.view.error.is_none() {
                    s.view.error = Some(message);
                    self.state.publish(&s);
                }
            }
            None if s.view.error.as_ref().is_some_and(|e| e.starts_with(PREFIX)) => {
                s.view.error = None;
                self.state.publish(&s);
            }
            None => (),
        }
    }

    pub fn update_state(&self) -> UpdateView {
        lock(&self.state.update).clone()
    }

    pub async fn check_update(&self) -> Result<UpdateView> {
        self.state.check_update(true).await
    }

    /// The window exits afterwards, so the update can be applied.
    pub async fn install_update(&self) -> Result<()> {
        const BUSY: &str = "Stop recording before updating";
        let state = &self.state;
        let recording = || busy(lock(&state.session).view.phase);
        anyhow::ensure!(!recording(), BUSY);
        let updater = {
            let mut update = lock(&state.update);
            let updater = state.updater.clone();
            anyhow::ensure!(
                update.phase == "available" && updater.is_some(),
                "Update unavailable"
            );
            update.phase = "installing";
            state.emit(Event::Update(update.clone()));
            updater.unwrap()
        };
        let result = async {
            // A recording started meanwhile would be lost on restart.
            anyhow::ensure!(!recording(), BUSY);
            tokio::task::spawn_blocking(move || updater.install()).await?
        }
        .await;
        if let Err(error) = &result {
            let message = error.to_string();
            state.change_update(|view| {
                if message == BUSY {
                    view.phase = "available";
                } else {
                    view.phase = "error";
                    view.error = Some(message.clone());
                }
            });
        }
        result
    }

    pub fn shutdown(&self) {
        self.state.s1.stop();
    }
}

/// Where Transcribe keeps its data, the same folder every earlier version used.
pub fn data_dir() -> Result<PathBuf> {
    const IDENTIFIER: &str = "app.transcribe.desktop";
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            System::Com::CoTaskMemFree,
            UI::Shell::{FOLDERID_RoamingAppData, SHGetKnownFolderPath},
        };
        let mut path = std::ptr::null_mut();
        // SAFETY: the out pointer receives a CoTaskMem string, freed below.
        let result = unsafe {
            SHGetKnownFolderPath(&FOLDERID_RoamingAppData, 0, std::ptr::null_mut(), &mut path)
        };
        anyhow::ensure!(result >= 0, "App data folder unavailable");
        let length = (0..).take_while(|&i| unsafe { *path.add(i) } != 0).count();
        let roaming = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(path, length) });
        unsafe { CoTaskMemFree(path as _) };
        Ok(PathBuf::from(roaming).join(IDENTIFIER))
    }
    #[cfg(not(windows))]
    {
        if let Some(data) =
            std::env::var_os("XDG_DATA_HOME").filter(|data| Path::new(data).is_absolute())
        {
            return Ok(PathBuf::from(data).join(IDENTIFIER));
        }
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .context("App data folder unavailable")?;
        Ok(PathBuf::from(home).join(".local/share").join(IDENTIFIER))
    }
}
