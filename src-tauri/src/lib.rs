mod focus;
mod live;
mod update;
mod widget;

use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, State,
};
use tauri_plugin_dialog::DialogExt;
use tokio_util::sync::CancellationToken;
use transcribe_core::{
    audio::{self, Recorder},
    credentials, hotkey, insertion,
    provider::{self, Audio, CleanupEngine, Registry},
    s1,
    storage::{Entry, Settings, Store},
};

type Result<T> = std::result::Result<T, String>;
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
pub(crate) fn busy(phase: &str) -> bool {
    matches!(
        phase,
        "starting" | "recording" | "transcribing" | "cleaning" | "inserting"
    )
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionView {
    phase: String,
    started_at: Option<i64>,
    error: Option<String>,
    retrying: bool,
    // The saved transcript the widget offers for dragging after paste fails.
    transcript: Option<String>,
}
struct Session {
    view: SessionView,
    id: String,
    recorder: Option<Recorder>,
    automatic: bool,
    widget_requested: bool,
    cancel: CancellationToken,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            view: SessionView {
                phase: "idle".into(),
                started_at: None,
                error: None,
                retrying: false,
                transcript: None,
            },
            id: String::new(),
            recorder: None,
            automatic: false,
            widget_requested: false,
            cancel: CancellationToken::new(),
        }
    }
}
struct AppState {
    store: Mutex<Store>,
    session: tokio::sync::Mutex<Session>,
    registry: Registry,
    live: tokio::sync::Mutex<live::LiveState>,
    api_key: PathBuf,
    hotkeys: Arc<hotkey::Service>,
    settings_lock: tokio::sync::Mutex<()>,
    s1: Arc<s1::Engine>,
}
impl AppState {
    fn emit(&self, app: &AppHandle, session: &Session) {
        self.hotkeys.dismissible(
            session.widget_requested
                && matches!(
                    session.view.phase.as_str(),
                    "starting" | "recording" | "transcribing" | "cleaning"
                ),
        );
        let _ = app.emit("session", &session.view);
        #[cfg(target_os = "macos")]
        queue_widget(app, false);
    }
    async fn error(&self, app: &AppHandle, id: &str, message: String) {
        let mut s = self.session.lock().await;
        if s.id != id || s.cancel.is_cancelled() {
            return;
        }
        s.recorder.take();
        s.view.phase = "error".into();
        s.view.error = Some(message);
        self.emit(app, &s);
    }
    // Ends a session that finished with nothing to show, such as silence.
    async fn dismiss(&self, app: &AppHandle, id: &str) {
        let mut s = self.session.lock().await;
        if s.id != id {
            return;
        }
        *s = Session::default();
        self.emit(app, &s);
        close_widget(app);
    }
}
/// Hides the widget once the pill has played its closing animation, unless a
/// new session has opened it again in the meantime.
fn close_widget(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        #[cfg(not(target_os = "macos"))]
        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        let state = app.state::<Arc<AppState>>().inner().clone();
        if state.session.lock().await.view.phase != "idle" {
            return;
        }
        if let Some(w) = app.get_webview_window("widget") {
            let _ = widget::hide(&w);
        }
    });
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Bootstrap {
    entries: Vec<Entry>,
    settings: Settings,
    microphones: Vec<String>,
    has_key: bool,
    session: SessionView,
}
fn main_only(window: &tauri::WebviewWindow) -> Result<()> {
    if window.label() != "main" {
        return Err("Unavailable".into());
    }
    Ok(())
}
#[tauri::command]
async fn bootstrap(state: State<'_, Arc<AppState>>) -> Result<Bootstrap> {
    let entries = state.store.lock().map_err(err)?.list().map_err(err)?;
    let mut settings = state.store.lock().map_err(err)?.settings();
    if hotkey::platform() == "linux-evdev" {
        settings.shortcut_label = None;
    }
    let path = state.api_key.clone();
    let (microphones, has_key) = tauri::async_runtime::spawn_blocking(move || {
        (
            audio::devices().unwrap_or_default(),
            credentials::read(&path).is_ok(),
        )
    })
    .await
    .map_err(err)?;
    Ok(Bootstrap {
        entries,
        settings,
        microphones,
        has_key,
        session: state.session.lock().await.view.clone(),
    })
}
#[tauri::command]
fn history(window: tauri::WebviewWindow, state: State<'_, Arc<AppState>>) -> Result<Vec<Entry>> {
    main_only(&window)?;
    state.store.lock().map_err(err)?.list().map_err(err)
}
#[tauri::command]
async fn copy_entry(
    window: tauri::WebviewWindow,
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    main_only(&window)?;
    let text = state
        .store
        .lock()
        .map_err(err)?
        .list()
        .map_err(err)?
        .into_iter()
        .find(|e| e.id == id)
        .ok_or("Transcript unavailable")?
        .text;
    insertion::copy(text).await.map_err(err)
}
#[tauri::command]
fn edit_entry(
    window: tauri::WebviewWindow,
    id: String,
    text: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    main_only(&window)?;
    if text.len() > 4 * 1024 * 1024 {
        return Err("Transcript too large".into());
    }
    state
        .store
        .lock()
        .map_err(err)?
        .edit(&id, &text)
        .map_err(err)
}
#[tauri::command]
async fn delete_entry(
    window: tauri::WebviewWindow,
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    main_only(&window)?;
    {
        let s = state.session.lock().await;
        if s.id == id && busy(&s.view.phase) {
            return Err("Transcription in progress".into());
        }
    }
    state.store.lock().map_err(err)?.delete(&id).map_err(err)?;
    Ok(())
}
#[derive(Serialize)]
struct CaptureSession {
    token: u64,
    platform: &'static str,
}
#[tauri::command]
async fn begin_shortcut_capture(
    window: tauri::WebviewWindow,
    state: State<'_, Arc<AppState>>,
) -> Result<CaptureSession> {
    main_only(&window)?;
    if !focus::is_active(&window.as_ref().window()).map_err(err)? {
        return Err("Focus Settings to record a shortcut".into());
    }
    if busy(&state.session.lock().await.view.phase) {
        return Err("Stop recording before changing the shortcut".into());
    }
    let service = state.hotkeys.clone();
    let token = tauri::async_runtime::spawn_blocking(move || service.begin_capture())
        .await
        .map_err(err)?
        .map_err(err)?;
    if !focus::is_active(&window.as_ref().window()).unwrap_or(false) {
        state.hotkeys.cancel_capture(Some(token));
        return Err("Focus Settings to record a shortcut".into());
    }
    let service = state.hotkeys.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        service.cancel_capture(Some(token));
    });
    Ok(CaptureSession {
        token,
        platform: hotkey::platform(),
    })
}
#[tauri::command]
fn capture_shortcut_key(
    window: tauri::WebviewWindow,
    token: u64,
    key: hotkey::Key,
    down: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<hotkey::Event>> {
    main_only(&window)?;
    if !focus::is_active(&window.as_ref().window()).map_err(err)? {
        state.hotkeys.cancel_capture(Some(token));
        return Ok(Vec::new());
    }
    state.hotkeys.capture_key(token, key, down).map_err(err)
}
#[tauri::command]
fn end_shortcut_capture(
    window: tauri::WebviewWindow,
    token: u64,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    main_only(&window)?;
    state.hotkeys.cancel_capture(Some(token));
    Ok(())
}
#[tauri::command]
async fn cleanup_models(state: State<'_, Arc<AppState>>) -> Result<Vec<provider::CleanupModel>> {
    let provider = state.registry.resolve(provider::MAI).map_err(err)?;
    provider.cleanup_models().await.map_err(err)
}

fn unload(settings: &Settings) -> Option<std::time::Duration> {
    settings
        .cleanup_unload_seconds
        .map(std::time::Duration::from_secs)
}
#[tauri::command]
async fn s1_status(state: State<'_, Arc<AppState>>) -> Result<s1::Status> {
    Ok(state.s1.status())
}
#[tauri::command]
fn download_s1(
    window: tauri::WebviewWindow,
    part: s1::Part,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    main_only(&window)?;
    let engine = state.s1.clone();
    // Progress and failures reach Settings as `s1` events.
    tauri::async_runtime::spawn(async move { engine.download(part).await });
    Ok(())
}
#[tauri::command]
fn cancel_s1_download(
    window: tauri::WebviewWindow,
    part: s1::Part,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    main_only(&window)?;
    state.s1.cancel_download(part);
    Ok(())
}
#[tauri::command]
async fn save_settings(
    window: tauri::WebviewWindow,
    app: AppHandle,
    mut settings: Settings,
    key: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Settings> {
    main_only(&window)?;
    settings.cleanup_model = settings.cleanup_model.trim().to_owned();
    if settings.cleanup_model.is_empty() {
        if settings.cleanup_engine == CleanupEngine::OpenRouter {
            return Err("Cleanup model required".into());
        }
        settings.cleanup_model = provider::DEFAULT_CLEANUP_MODEL.into();
    }
    let _guard = state.settings_lock.lock().await;
    let old = state.store.lock().map_err(err)?.settings();
    state.hotkeys.validate(&settings.shortcut).map_err(err)?;
    if let Some(key) = key {
        let path = state.api_key.clone();
        tauri::async_runtime::spawn_blocking(move || credentials::save(&path, &key))
            .await
            .map_err(err)?
            .map_err(err)?;
    }
    settings.shortcut_label = register_shortcut(&app, &settings.shortcut).await?;
    let saved = state
        .store
        .lock()
        .map_err(err)?
        .save_settings(&settings)
        .map_err(err);
    if let Err(message) = saved {
        if old.shortcut != settings.shortcut {
            if let Err(rollback) = register_shortcut(&app, &old.shortcut).await {
                return Err(format!("{message}; shortcut restore failed: {rollback}"));
            }
        }
        return Err(message);
    }
    state.s1.set_unload(unload(&settings));
    if settings.cleanup_engine == CleanupEngine::OpenRouter {
        state.s1.stop();
    }
    Ok(settings)
}
fn show_history(app: &AppHandle) {
    let _ = app.emit_to("main", "open-history", ());
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}
#[tauri::command]
async fn open_history(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<()> {
    open_history_impl(app, state.inner().clone()).await;
    Ok(())
}
async fn open_history_impl(app: AppHandle, state: Arc<AppState>) {
    state.session.lock().await.widget_requested = false;
    show_history(&app);
    if let Some(w) = app.get_webview_window("widget") {
        let _ = widget::hide(&w);
    }
}
fn show_widget(app: &AppHandle) {
    queue_widget(app, false);
}
static WIDGET_REFRESH_PENDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
fn queue_widget(app: &AppHandle, active_only: bool) {
    use std::sync::atomic::Ordering;
    // Coalesce move events while the UI thread is busy, retaining the latest
    // destination (which is resolved when the queued update actually runs).
    if active_only && WIDGET_REFRESH_PENDING.swap(true, Ordering::AcqRel) {
        return;
    }
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        if active_only {
            WIDGET_REFRESH_PENDING.store(false, Ordering::Release);
        }
        let state = handle.state::<Arc<AppState>>();
        // Recheck on the UI thread so a queued show cannot undo cancellation.
        let Ok(mut session) = state.session.try_lock() else {
            // State changes can still hold the lock when AppKit handles this
            // event. Retry from the latest state instead of losing a terminal
            // update (or replaying an obsolete recording after cancellation).
            #[cfg(target_os = "macos")]
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(16)).await;
                queue_widget(&handle, active_only);
            });
            return;
        };
        if !session.widget_requested {
            #[cfg(target_os = "macos")]
            widget::macos::hide();
            return;
        }
        if !busy(&session.view.phase)
            && (active_only || !matches!(session.view.phase.as_str(), "error" | "done"))
        {
            return;
        }
        let result = handle
            .get_webview_window("widget")
            .ok_or_else(|| "Widget window unavailable".to_string())
            .and_then(|w| widget::show(&w, &session.view));
        match result {
            Err(error) => {
                let message = format!("Recording widget: {error}");
                eprintln!("{message}");
                if session.view.error.is_none() {
                    session.view.error = Some(message);
                    state.emit(&handle, &session);
                }
            }
            Ok(())
                if session
                    .view
                    .error
                    .as_deref()
                    .is_some_and(|e| e.starts_with("Recording widget: ")) =>
            {
                session.view.error = None;
                state.emit(&handle, &session);
            }
            Ok(()) => {}
        }
    }) {
        if active_only {
            WIDGET_REFRESH_PENDING.store(false, Ordering::Release);
        }
        eprintln!("Widget UI dispatch failed: {error}");
    }
}
#[tauri::command]
async fn toggle(
    window: tauri::WebviewWindow,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    toggle_impl(app, state.inner().clone(), window.label() != "main").await
}
async fn toggle_impl(app: AppHandle, state: Arc<AppState>, automatic: bool) -> Result<()> {
    let mut s = state.session.lock().await;
    if state.live.lock().await.view.phase.active() {
        return Err("Stop the live session first".into());
    }
    if s.view.phase == "recording" {
        drop(s);
        return stop_recording(app, state, None).await;
    }
    if busy(&s.view.phase) {
        return Ok(());
    }
    // A shortcut started in our own window has the same save-only intent as
    // the Record button. External shortcuts still resolve their destination
    // at delivery; no application or field is captured here.
    let automatic = automatic
        && !app
            .get_webview_window("main")
            .is_some_and(|window| focus::is_active(&window.as_ref().window()).unwrap_or(false));
    let id = uuid::Uuid::new_v4().to_string();
    *s = Session {
        id: id.clone(),
        widget_requested: true,
        view: SessionView {
            phase: "starting".into(),
            started_at: None,
            error: None,
            retrying: false,
            transcript: None,
        },
        ..Default::default()
    };
    state.emit(&app, &s);
    drop(s);
    show_widget(&app);
    if automatic {
        tauri::async_runtime::spawn(insertion::prepare());
    }
    let task = async {
        let path = state.api_key.clone();
        let has_key = tauri::async_runtime::spawn_blocking(move || credentials::read(&path))
            .await
            .map_err(err)?
            .is_ok();
        if !has_key {
            return Err::<(), String>("API key required".into());
        }
        let settings = state.store.lock().map_err(err)?.settings();
        if settings.cleanup_engine == CleanupEngine::S1Mini {
            state.s1.check().map_err(err)?;
            // The model loads while the user is still speaking.
            let engine = state.s1.clone();
            tauri::async_runtime::spawn(async move { engine.warm().await });
        }
        let microphone = settings.microphone;
        let events = app.clone();
        let recorder = tauri::async_runtime::spawn_blocking(move || {
            Recorder::start(
                microphone,
                Arc::new(move |n| {
                    let _ = events.emit("level", n);
                    #[cfg(target_os = "macos")]
                    widget::macos::level(n);
                }),
            )
        })
        .await
        .map_err(err)?
        .map_err(err)?;
        let mut s = state.session.lock().await;
        if s.id != id || s.cancel.is_cancelled() {
            return Ok(());
        }
        s.recorder = Some(recorder);
        s.automatic = automatic;
        s.view.phase = "recording".into();
        s.view.started_at = Some(now());
        state.emit(&app, &s);
        drop(s);
        show_widget(&app);
        let timer_app = app.clone();
        let timer_state = state.clone();
        let timer_id = id.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(audio::MAX_SECONDS)).await;
            let _ = stop_recording(timer_app, timer_state, Some(timer_id)).await;
        });
        Ok(())
    };
    if let Err(message) = task.await {
        state.error(&app, &id, message.clone()).await;
        show_widget(&app);
        return Err(message);
    }
    Ok(())
}
async fn stop_recording(
    app: AppHandle,
    state: Arc<AppState>,
    expected: Option<String>,
) -> Result<()> {
    let mut s = state.session.lock().await;
    if s.view.phase != "recording" || expected.as_ref().is_some_and(|id| *id != s.id) {
        return Ok(());
    }
    let recorder = s.recorder.take().ok_or("Recording unavailable")?;
    s.view.phase = "transcribing".into();
    state.emit(&app, &s);
    let id = s.id.clone();
    let automatic = s.automatic;
    let cancel = s.cancel.clone();
    drop(s);
    tauri::async_runtime::spawn(async move {
        match tauri::async_runtime::spawn_blocking(move || recorder.finish()).await {
            Ok(Ok((audio, seconds))) => {
                process(
                    app,
                    state,
                    Job {
                        id,
                        audio,
                        seconds,
                        automatic,
                        cancel,
                    },
                )
                .await
            }
            Ok(Err(error)) if error.is::<audio::NoSpeech>() => state.dismiss(&app, &id).await,
            result => {
                state
                    .error(
                        &app,
                        &id,
                        match result {
                            Ok(Err(e)) => err(e),
                            _ => "Recording failed".into(),
                        },
                    )
                    .await
            }
        }
    });
    Ok(())
}
#[tauri::command]
async fn cancel(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<()> {
    cancel_impl(app, state.inner().clone()).await
}
async fn cancel_impl(app: AppHandle, state: Arc<AppState>) -> Result<()> {
    let mut s = state.session.lock().await;
    if s.view.phase == "inserting" {
        return Ok(());
    }
    s.cancel.cancel();
    s.recorder.take();
    *s = Session::default();
    state.emit(&app, &s);
    close_widget(&app);
    Ok(())
}
struct Job {
    id: String,
    audio: Audio,
    seconds: f64,
    automatic: bool,
    cancel: CancellationToken,
}
async fn process(app: AppHandle, state: Arc<AppState>, job: Job) {
    let Job {
        id,
        audio,
        seconds,
        automatic,
        cancel,
    } = job;
    let operation = async {
        if cancel.is_cancelled() {
            return Err("Cancelled".into());
        }
        state
            .store
            .lock()
            .map_err(err)?
            .insert(&Entry {
                id: id.clone(),
                created_at: now(),
                text: String::new(),
                seconds,
                status: "transcribing".into(),
                error: None,
            })
            .map_err(err)?;
        let _ = app.emit("history", ());
        let provider = state.registry.resolve(provider::MAI).map_err(err)?;
        let settings = state.store.lock().map_err(err)?.settings();
        let cleanup = match settings.cleanup_engine {
            CleanupEngine::OpenRouter => provider::CleanupConfig::OpenRouter {
                model: settings.cleanup_model,
                reasoning_effort: settings.cleanup_reasoning_effort,
            },
            CleanupEngine::S1Mini => provider::CleanupConfig::Local {
                engine: state.s1.clone(),
                styling: settings.cleanup_styling,
            },
        };
        let path = state.api_key.clone();
        let key = tauri::async_runtime::spawn_blocking(move || credentials::read(&path))
            .await
            .map_err(err)?
            .map_err(err)?;
        // One consumer applies progress in order; it never regresses a session
        // that has already moved on.
        let (progress, mut updates) = tokio::sync::mpsc::unbounded_channel();
        {
            let (app, state, id) = (app.clone(), state.clone(), id.clone());
            tauri::async_runtime::spawn(async move {
                while let Some(update) = updates.recv().await {
                    let mut s = state.session.lock().await;
                    if s.id != id || s.cancel.is_cancelled() {
                        break;
                    }
                    match update {
                        provider::Progress::Cleaning if s.view.phase == "transcribing" => {
                            s.view.phase = "cleaning".into();
                            s.view.retrying = false;
                        }
                        provider::Progress::Retrying
                            if matches!(s.view.phase.as_str(), "transcribing" | "cleaning") =>
                        {
                            s.view.retrying = true;
                        }
                        _ => continue,
                    }
                    state.emit(&app, &s);
                }
            });
        }
        let result = match provider
            .transcribe(
                provider::MAI,
                audio,
                &key,
                &cleanup,
                &move |update| {
                    let _ = progress.send(update);
                },
                cancel.clone(),
            )
            .await
        {
            // Silence is not a failure, and leaves nothing to keep.
            Err(error) if error.is::<audio::NoSpeech>() => {
                state.store.lock().map_err(err)?.delete(&id).map_err(err)?;
                let _ = app.emit("history", ());
                state.dismiss(&app, &id).await;
                return Ok(());
            }
            result => result.map_err(err)?,
        };
        state
            .store
            .lock()
            .map_err(err)?
            .edit(&id, &result.text)
            .map_err(err)?;
        state
            .store
            .lock()
            .map_err(err)?
            .status(&id, "pending", None)
            .map_err(err)?;
        let _ = app.emit("history", ());
        // Persist first: insertion failures must never lose a successful transcript.
        let mut s = state.session.lock().await;
        if cancel.is_cancelled() {
            return Err("Cancelled".into());
        }
        if s.id != id {
            drop(s);
            state
                .store
                .lock()
                .map_err(err)?
                .status(&id, "saved", None)
                .map_err(err)?;
            let _ = app.emit("history", ());
            return Ok(());
        }
        s.view.phase = "inserting".into();
        s.view.retrying = false;
        state.emit(&app, &s);
        drop(s);
        let hotkey_pause = state.hotkeys.pause();
        let insertion = if automatic {
            insertion::insert(&id, &result.text).await.map_err(err)
        } else {
            Ok(())
        };
        drop(hotkey_pause);
        let status = if insertion.is_ok() && automatic {
            "unverified"
        } else {
            "saved"
        };
        state
            .store
            .lock()
            .map_err(err)?
            .status(&id, status, insertion.as_ref().err().map(String::as_str))
            .map_err(err)?;
        let _ = app.emit("history", ());
        let mut s = state.session.lock().await;
        if s.id == id {
            s.view.phase = if insertion.is_ok() { "done" } else { "error" }.into();
            s.view.transcript = insertion.is_err().then(|| result.text.clone());
            s.view.error = insertion.err();
            state.emit(&app, &s);
            let unpasted = s.view.transcript.is_some();
            drop(s);
            // The widget grows into the transcript card.
            if unpasted {
                show_widget(&app);
            }
        }
        Ok::<(), String>(())
    };
    match operation.await {
        // A cancelled transcription leaves nothing behind.
        Err(_) if cancel.is_cancelled() => {
            let _ = state.store.lock().map(|store| store.delete(&id));
            let _ = app.emit("history", ());
        }
        Err(message) => {
            let _ = state
                .store
                .lock()
                .map(|store| store.status(&id, "failed", Some(&message)));
            let _ = app.emit("history", ());
            state.error(&app, &id, message).await;
        }
        Ok(()) => {}
    }
    // Long enough for the pill's check to land before it closes.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let mut s = state.session.lock().await;
    if s.id == id && s.view.phase == "done" {
        *s = Session::default();
        state.emit(&app, &s);
        close_widget(&app);
    }
}
#[tauri::command]
async fn import_audio(
    window: tauri::WebviewWindow,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    main_only(&window)?;
    let handle = app.clone();
    let path = tauri::async_runtime::spawn_blocking(move || {
        handle
            .dialog()
            .file()
            .add_filter("Audio", &["wav", "mp3", "flac"])
            .blocking_pick_file()
    })
    .await
    .map_err(err)?;
    let Some(path) = path else {
        return Ok(());
    };
    let path = path.into_path().map_err(err)?;
    let metadata = tokio::fs::metadata(&path).await.map_err(err)?;
    if metadata.len() > 64 * 1024 * 1024 {
        return Err("Audio is too large".into());
    }
    let format = path
        .extension()
        .and_then(|s| s.to_str())
        .ok_or("Audio format unsupported")?
        .to_lowercase();
    if !["wav", "mp3", "flac"].contains(&format.as_str()) {
        return Err("Audio format unsupported".into());
    }
    let bytes = tokio::fs::read(path).await.map_err(err)?;
    start_import(app, state.inner().clone(), Audio { bytes, format }).await
}
async fn start_import(app: AppHandle, state: Arc<AppState>, audio: Audio) -> Result<()> {
    let mut s = state.session.lock().await;
    if state.live.lock().await.view.phase.active() {
        return Err("Stop the live session first".into());
    }
    if busy(&s.view.phase) {
        return Err("Recording in progress".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    *s = Session {
        id: id.clone(),
        view: SessionView {
            phase: "transcribing".into(),
            started_at: None,
            error: None,
            retrying: false,
            transcript: None,
        },
        ..Default::default()
    };
    let cancel = s.cancel.clone();
    state.emit(&app, &s);
    drop(s);
    tauri::async_runtime::spawn(process(
        app,
        state,
        Job {
            id,
            audio,
            seconds: 0.,
            automatic: false,
            cancel,
        },
    ));
    Ok(())
}
async fn register_shortcut(app: &AppHandle, shortcut: &str) -> Result<Option<String>> {
    let service = app.state::<Arc<AppState>>().hotkeys.clone();
    let binding = service.validate(shortcut).map_err(err)?;
    tauri::async_runtime::spawn_blocking(move || {
        service.start().map_err(err)?;
        service.configure(binding);
        Ok::<(), String>(())
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    Ok(None)
}
fn install_state_and_windows<R: tauri::Runtime>(
    app: &tauri::App<R>,
    state: Arc<AppState>,
) -> tauri::Result<()> {
    // Configured windows opt out of Tauri's automatic creation: their frontend
    // can invoke bootstrap immediately, before the normal setup callback runs.
    app.manage(state);
    for config in &app.config().app.windows {
        let _window = tauri::WebviewWindowBuilder::from_config(app, config)?
            .initialization_script(format!(
                "window.__TRANSCRIBE_PLATFORM__ = {:?};",
                std::env::consts::OS
            ))
            .build()?;
        #[cfg(target_os = "linux")]
        if config.label == "widget" {
            widget::init_linux(&_window)?;
        }
    }
    Ok(())
}
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_history(app)
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            app.set_theme(Some(tauri::Theme::Dark));
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            transcribe_core::storage::remove_legacy_audio(&data)?;
            let store = Store::open(&data.join("history.sqlite"))?;
            let engine = s1::Engine::new(data.join("s1"));
            engine.set_unload(unload(&store.settings()));
            let mut downloads = engine.subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while downloads.changed().await.is_ok() {
                    let status = downloads.borrow_and_update().clone();
                    let _ = handle.emit_to("main", "s1", status);
                }
            });
            let handle = app.handle().clone();
            let hotkeys = hotkey::Service::new(move |event| match event {
                hotkey::Event::Activate => {
                    let app = handle.clone();
                    let state = app.state::<Arc<AppState>>().inner().clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = toggle_impl(app, state, true).await;
                    });
                }
                hotkey::Event::Dismiss => {
                    let app = handle.clone();
                    let state = app.state::<Arc<AppState>>().inner().clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = cancel_impl(app, state).await;
                    });
                }
                hotkey::Event::Error { message } => {
                    let app = handle.clone();
                    tauri::async_runtime::spawn(async move {
                        let state = app.state::<Arc<AppState>>();
                        let mut session = state.session.lock().await;
                        session.view.error = Some(message);
                        state.emit(&app, &session);
                    });
                }
                capture => {
                    let _ = handle.emit_to("main", "shortcut-capture", capture);
                }
            });
            install_state_and_windows(
                app,
                Arc::new(AppState {
                    store: Mutex::new(store),
                    session: tokio::sync::Mutex::new(Session::default()),
                    registry: Registry::default(),
                    live: tokio::sync::Mutex::new(live::LiveState::default()),
                    api_key: data.join("api-key"),
                    hotkeys,
                    settings_lock: tokio::sync::Mutex::new(()),
                    s1: engine,
                }),
            )?;
            app.manage(update::UpdateState::new(
                app.package_info().version.to_string(),
            ));
            update::watch(app.handle().clone());
            #[cfg(target_os = "macos")]
            widget::macos::init(app.handle().clone());
            // A hidden widget or display change must not leave a live recording
            // invisible. All native show/position work stays on the UI thread.
            #[cfg(windows)]
            {
                widget::watch_foreground(app.handle().clone()).map_err(std::io::Error::other)?;
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
                        queue_widget(&handle, true);
                    }
                });
            }
            let history = MenuItem::with_id(app, "history", "History", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&history, &quit])?;
            let mut tray = TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("Transcribe")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "history" => show_history(app),
                    "quit" => app.exit(0),
                    _ => (),
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<Arc<AppState>>();
                let _guard = state.settings_lock.lock().await;
                let shortcut = state.store.lock().unwrap().settings().shortcut;
                if let Err(message) = register_shortcut(&handle, &shortcut).await {
                    let state = handle.state::<Arc<AppState>>();
                    let mut s = state.session.lock().await;
                    s.view.error = Some(message);
                    state.emit(&handle, &s);
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                let cancel_capture = match event {
                    tauri::WindowEvent::Focused(false) => {
                        !focus::is_active(window).unwrap_or(false)
                    }
                    tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed => {
                        true
                    }
                    _ => false,
                };
                if cancel_capture {
                    window.state::<Arc<AppState>>().hotkeys.cancel_capture(None);
                }
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            history,
            copy_entry,
            edit_entry,
            delete_entry,
            save_settings,
            cleanup_models,
            s1_status,
            download_s1,
            cancel_s1_download,
            begin_shortcut_capture,
            capture_shortcut_key,
            end_shortcut_capture,
            open_history,
            toggle,
            cancel,
            import_audio,
            live::live_bootstrap,
            live::start_live,
            live::stop_live,
            live::cancel_live,
            live::save_live_keys,
            live::copy_live,
            live::delete_live,
            live::retry_live_summary,
            update::update_state,
            update::check_update,
            update::install_update
        ])
        .build(tauri::generate_context!())
        .expect("Transcribe failed to start")
        .run(|app, event| {
            if let (tauri::RunEvent::Exit, Some(state)) = (event, app.try_state::<Arc<AppState>>())
            {
                state.s1.stop();
            }
        });
}
