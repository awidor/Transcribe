//! The main window: a top bar with History and Settings, and toasts.
use crate::{
    controls::{self, Tone, button, icon, tabs, tabular},
    shell::Shell,
    theme,
};
use gpui::{
    Animation, AnimationExt, App, AppContext, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, ParentElement, PathPromptOptions, Render, SharedString,
    Styled, Subscription, Task, UniformListScrollHandle, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::{
    IndexPath, TitleBar,
    input::{InputEvent, InputState, TextareaState},
    select::{SelectEvent, SelectState},
};
use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::runtime::Handle;
use transcribe_core::{
    hotkey,
    provider::{CleanupEngine, CleanupModel, ReasoningEffort, TranscriptionModel},
    rules::{self, RecorderMode, RecorderResult, ShortcutRecorder, StreamingChoice},
    s1,
    session::{App as Core, Bootstrap, Phase, Session, UpdateView},
    storage::{Entry, Settings},
};

pub type Options = Vec<SharedString>;

/// Cleanup engines in the order the Cleanup control lists them.
pub const ENGINES: [(CleanupEngine, &str); 4] = [
    (CleanupEngine::Off, "Off"),
    (CleanupEngine::OpenRouter, "OpenRouter"),
    (CleanupEngine::S1Mini, "S1-mini by Superwhisper"),
    (CleanupEngine::MaiTranscribe, "MAI-Transcribe-2"),
];

pub const MODELS: [(TranscriptionModel, &str); 2] = [
    (TranscriptionModel::MaiTranscribe, "MAI-Transcribe-2"),
    (TranscriptionModel::Voxtral, "Voxtral Small Transcribe 3"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    History,
    Settings,
}

/// What a main window shares with the shell.
pub struct Parts {
    pub shell: gpui::WeakEntity<Shell>,
    pub app: Core,
    pub runtime: Handle,
    pub focused: Arc<AtomicBool>,
    #[cfg(windows)]
    pub keys: Arc<crate::windows::KeyRouter>,
}

pub struct MainWindow {
    pub(crate) shell: gpui::WeakEntity<Shell>,
    #[cfg(windows)]
    keys: Arc<crate::windows::KeyRouter>,
    pub(crate) app: Core,
    pub(crate) runtime: Handle,
    focused: Arc<AtomicBool>,
    pub(crate) tab: Tab,
    pub(crate) loaded: bool,
    pub(crate) session: Session,
    pub(crate) has_azure_key: bool,
    pub(crate) has_openrouter_key: bool,
    command_error: Option<String>,
    toast: Option<(String, bool)>,
    saved: Option<Task<()>>,
    pub(crate) update: UpdateView,
    clock: Option<Task<()>>,

    pub(crate) entries: Vec<Entry>,
    pub(crate) history_failed: bool,
    pub(crate) selected: Option<String>,
    shown_text: String,
    pub(crate) search: Entity<InputState>,
    pub(crate) text: Entity<TextareaState>,
    pub(crate) copied: Option<Task<()>>,
    pub(crate) list_scroll: UniformListScrollHandle,

    pub(crate) draft: Settings,
    choice: StreamingChoice,
    pub(crate) microphones: Vec<String>,
    pub(crate) azure_key: Entity<InputState>,
    pub(crate) openrouter_key: Entity<InputState>,
    pub(crate) model: Entity<InputState>,
    pub(crate) models_search: Entity<InputState>,
    pub(crate) models: Vec<CleanupModel>,
    pub(crate) loading_models: bool,
    pub(crate) models_failed: bool,
    pub(crate) models_open: bool,
    pub(crate) efforts: Vec<ReasoningEffort>,
    pub(crate) microphone: Entity<SelectState<Options>>,
    pub(crate) transcription: Entity<SelectState<Options>>,
    pub(crate) engine: Entity<SelectState<Options>>,
    pub(crate) effort: Entity<SelectState<Options>>,
    pub(crate) styling: Entity<SelectState<Options>>,
    pub(crate) unload: Entity<SelectState<Options>>,
    pub(crate) s1: s1::Status,
    pub(crate) recorder: ShortcutRecorder,
    pub(crate) shortcut_error: Option<String>,
    capture_timer: Option<Task<()>>,
    pub(crate) saving: bool,

    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

fn options(labels: impl IntoIterator<Item = impl Into<SharedString>>) -> Options {
    labels.into_iter().map(Into::into).collect()
}

fn select(
    labels: Options,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) -> Entity<SelectState<Options>> {
    cx.new(|cx| SelectState::new(labels, Some(IndexPath::new(0)), window, cx))
}

fn pick(state: &Entity<SelectState<Options>>, index: usize, window: &mut Window, cx: &mut App) {
    state.update(cx, |state, cx| {
        state.set_selected_index(Some(IndexPath::new(index)), window, cx)
    });
}

/// Where the chosen label sits among `labels`.
fn chosen(event: &SelectEvent<Options>, labels: &[SharedString]) -> Option<usize> {
    let SelectEvent::Confirm(Some(value)) = event else {
        return None;
    };
    labels.iter().position(|label| label == value)
}

/// Runs blocking core work on the runtime's blocking pool.
pub fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> impl Future<Output = anyhow::Result<T>> + Send + 'static {
    async move { tokio::task::spawn_blocking(f).await? }
}

pub fn date(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|time| time.format("%b %-d, %H:%M").to_string())
        .unwrap_or_default()
}

impl MainWindow {
    pub fn new(parts: Parts, tab: Tab, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Parts {
            shell,
            app,
            runtime,
            focused,
            #[cfg(windows)]
            keys,
        } = parts;
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search transcripts"));
        let text = cx.new(|cx| TextareaState::new(window, cx));
        let azure_key = cx.new(|cx| InputState::new(window, cx).masked(true));
        let openrouter_key = cx.new(|cx| InputState::new(window, cx).masked(true));
        let model = cx.new(|cx| InputState::new(window, cx));
        let models_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models"));
        let microphone = select(options(["System default"]), window, cx);
        let transcription = select(options(MODELS.map(|(_, name)| name)), window, cx);
        let engine = select(options(ENGINES.map(|(_, name)| name)), window, cx);
        let effort = select(options(["Model default"]), window, cx);
        let styling = select(
            options(rules::STYLINGS.map(rules::styling_label)),
            window,
            cx,
        );
        let unload = select(
            options(rules::UNLOAD_CHOICES.iter().map(|choice| choice.label)),
            window,
            cx,
        );

        let mut subscriptions = vec![
            cx.subscribe_in(&search, window, |this, _, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.list_scroll
                        .scroll_to_item(0, gpui::ScrollStrategy::Top);
                    cx.notify();
                }
            }),
            cx.subscribe_in(&text, window, |this, _, event, window, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::Blur => this.save_text(window, cx),
                _ => {}
            }),
            cx.subscribe_in(&model, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let model = state.read(cx).value().to_string();
                    if model != this.draft.cleanup_model {
                        this.draft.cleanup_reasoning_effort = None;
                    }
                    this.draft.cleanup_model = model;
                    cx.notify();
                }
            }),
            cx.subscribe_in(&models_search, window, |_, _, _: &InputEvent, _, cx| {
                cx.notify()
            }),
            cx.subscribe_in(&microphone, window, |this, _, event, _, cx| {
                let labels = this.microphone_labels();
                if let Some(index) = chosen(event, &labels) {
                    this.draft.microphone = index
                        .checked_sub(1)
                        .and_then(|index| this.microphones.get(index).cloned());
                    cx.notify();
                }
            }),
            cx.subscribe_in(&transcription, window, |this, _, event, window, cx| {
                let labels = options(MODELS.map(|(_, name)| name));
                if let Some(index) = chosen(event, &labels) {
                    this.choice.model(&mut this.draft, MODELS[index].0);
                    this.show_cleanup(window, cx);
                }
            }),
            cx.subscribe_in(&engine, window, |this, _, event, window, cx| {
                let labels = options(ENGINES.map(|(_, name)| name));
                if let Some(index) = chosen(event, &labels) {
                    this.choice.cleanup(&mut this.draft, ENGINES[index].0);
                    this.show_cleanup(window, cx);
                }
            }),
            cx.subscribe_in(&effort, window, |this, _, event, _, cx| {
                let labels = this.effort_labels();
                if let Some(index) = chosen(event, &labels) {
                    this.draft.cleanup_reasoning_effort = index
                        .checked_sub(1)
                        .and_then(|index| this.efforts.get(index).copied());
                    cx.notify();
                }
            }),
            cx.subscribe_in(&styling, window, |this, _, event, _, cx| {
                let labels = options(rules::STYLINGS.map(rules::styling_label));
                if let Some(index) = chosen(event, &labels) {
                    this.draft.cleanup_styling = rules::STYLINGS[index];
                    cx.notify();
                }
            }),
            cx.subscribe_in(&unload, window, |this, _, event, _, cx| {
                let labels = options(rules::UNLOAD_CHOICES.iter().map(|choice| choice.label));
                if let Some(index) = chosen(event, &labels) {
                    this.draft.cleanup_unload_seconds = rules::UNLOAD_CHOICES[index].seconds;
                    cx.notify();
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                let active = window.is_window_active();
                this.focused.store(active, Ordering::Relaxed);
                if !active {
                    this.stop_capture(cx);
                }
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                theme::apply(window.appearance(), cx);
            }),
        ];
        let release_focus = focused.clone();
        subscriptions.push(cx.on_release(move |this, cx| {
            release_focus.store(false, Ordering::Relaxed);
            if let Some(token) = this.recorder.stop() {
                this.app.end_shortcut_capture(Some(token));
            }
            let _ = cx;
        }));
        focused.store(window.is_window_active(), Ordering::Relaxed);
        theme::apply(window.appearance(), cx);

        let mut this = Self {
            shell,
            #[cfg(windows)]
            keys,
            update: app.update_state(),
            s1: app.s1_status(),
            app,
            runtime,
            focused,
            tab,
            loaded: false,
            session: Session::default(),
            has_azure_key: false,
            has_openrouter_key: false,
            command_error: None,
            toast: None,
            saved: None,
            clock: None,
            entries: Vec::new(),
            history_failed: false,
            selected: None,
            shown_text: String::new(),
            search,
            text,
            copied: None,
            list_scroll: UniformListScrollHandle::new(),
            draft: Settings::default(),
            choice: StreamingChoice::default(),
            microphones: Vec::new(),
            azure_key,
            openrouter_key,
            model,
            models_search,
            models: Vec::new(),
            loading_models: false,
            models_failed: false,
            models_open: false,
            efforts: Vec::new(),
            microphone,
            transcription,
            engine,
            effort,
            styling,
            unload,
            recorder: ShortcutRecorder::default(),
            shortcut_error: None,
            capture_timer: None,
            saving: false,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.bootstrap(window, cx);
        this.load_models(window, cx);
        this
    }

    /// Runs core work away from the UI, then hands its result to `done`, or
    /// its failure to `failed`.
    pub(crate) fn run_or<T: Send + 'static>(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        work: impl Future<Output = anyhow::Result<T>> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        failed: impl FnOnce(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let task = self.runtime.spawn(work);
        cx.spawn_in(window, async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(error.into()),
            };
            this.update_in(cx, |this, window, cx| match result {
                Ok(value) => done(this, value, window, cx),
                Err(error) => failed(this, error.to_string(), window, cx),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn run<T: Send + 'static>(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        work: impl Future<Output = anyhow::Result<T>> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.command_error = None;
        self.refresh_toast(cx);
        self.run_or(window, cx, work, done, |this, message, _, cx| {
            this.fail(message, cx)
        });
    }

    pub(crate) fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        self.command_error = Some(message);
        self.refresh_toast(cx);
    }

    fn bootstrap(&mut self, window: &Window, cx: &mut Context<Self>) {
        let app = self.app.clone();
        self.run(
            window,
            cx,
            blocking(move || Ok(app.bootstrap())),
            |this, bootstrap: Bootstrap, window, cx| {
                this.entries = bootstrap.entries;
                this.has_azure_key = bootstrap.has_azure_key;
                this.has_openrouter_key = bootstrap.has_open_router_key;
                this.microphones = bootstrap.microphones;
                this.load_settings(bootstrap.settings, window, cx);
                this.set_session(bootstrap.session, window, cx);
                if !this.has_azure_key && !this.loaded {
                    this.tab = Tab::Settings;
                }
                this.loaded = true;
                this.show_entries(window, cx);
            },
        );
    }

    pub fn show_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    pub fn set_session(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        let recording = session.phase == Phase::Recording;
        self.session = session;
        if recording && self.clock.is_none() {
            self.clock = Some(cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(250))
                        .await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
            }));
        } else if !recording {
            self.clock = None;
        }
        self.refresh_toast(cx);
        cx.notify();
    }

    pub fn update_view(&mut self, update: UpdateView, cx: &mut Context<Self>) {
        self.update = update;
        cx.notify();
    }

    pub fn set_s1(&mut self, status: s1::Status, cx: &mut Context<Self>) {
        self.s1 = status;
        cx.notify();
    }

    /// A failed paste is offered in the pill and kept in history, not reported here.
    fn refresh_toast(&mut self, cx: &mut Context<Self>) {
        let message = self.command_error.clone().or_else(|| {
            if self.session.transcript.is_some() {
                None
            } else {
                self.session.error.clone()
            }
        });
        let toast = message.map(|message| (message, self.command_error.is_none()));
        if toast != self.toast {
            self.toast = toast;
            cx.notify();
        }
    }

    fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        if let Some((_, from_session)) = self.toast.take() {
            if from_session {
                self.app.cancel();
            } else {
                self.command_error = None;
            }
        }
        cx.notify();
    }

    pub(crate) fn notice_saved(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.saved = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            this.update(cx, |this, cx| {
                this.saved = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(crate) fn import_audio(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                let app = this.app.clone();
                this.run(
                    window,
                    cx,
                    blocking(move || app.import_audio(&path)),
                    |_, _, _, _| {},
                );
            })
            .ok();
        })
        .detach();
    }

    fn record(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app = self.app.clone();
        self.run(
            window,
            cx,
            async move { app.toggle(false).await },
            |_, _, _, _| {},
        );
    }

    // Shortcut capture

    pub(crate) fn begin_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.recorder.mode() != RecorderMode::Idle || self.saving {
            return;
        }
        self.shortcut_error = None;
        if !window.is_window_active() {
            self.shortcut_error = Some(rules::FOCUS_SETTINGS.into());
            cx.notify();
            return;
        }
        // Keys being recorded must not reach a text field.
        window.focus(&self.focus, cx);
        self.recorder.starting();
        self.mirror_capture(cx);
        self.capture_timer = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(15))
                .await;
            this.update(cx, |this, cx| this.stop_capture(cx)).ok();
        }));
        let app = self.app.clone();
        self.run_or(
            window,
            cx,
            blocking(move || app.begin_shortcut_capture()),
            |this, capture, window, cx| {
                if this.recorder.mode() == RecorderMode::Starting && !window.is_window_active() {
                    this.stop_capture(cx);
                    this.shortcut_error = Some(rules::FOCUS_SETTINGS.into());
                }
                let result = this.recorder.begin(capture.token, capture.platform);
                this.captured(result, cx);
            },
            |this, message, _, cx| {
                if this.recorder.mode() == RecorderMode::Starting {
                    this.stop_capture(cx);
                }
                this.shortcut_error = Some(message);
                cx.notify();
            },
        );
        cx.notify();
    }

    pub fn capture(&mut self, event: hotkey::Event, cx: &mut Context<Self>) {
        let result = self.recorder.receive(event);
        self.captured(result, cx);
    }

    pub fn capture_failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.stop_capture(cx);
        self.shortcut_error = Some(message);
        cx.notify();
    }

    fn captured(&mut self, result: RecorderResult, cx: &mut Context<Self>) {
        if result.stopped {
            self.capture_timer = None;
            if let Some(token) = result.ended {
                let app = self.app.clone();
                self.runtime
                    .spawn_blocking(move || app.end_shortcut_capture(Some(token)));
            }
            if let Some(shortcut) = result.shortcut {
                self.draft.shortcut = shortcut;
                self.draft.shortcut_label = None;
            }
        }
        self.mirror_capture(cx);
        cx.notify();
    }

    pub(crate) fn stop_capture(&mut self, cx: &mut Context<Self>) {
        if self.recorder.mode() == RecorderMode::Idle {
            return;
        }
        // A capture still starting ends when its token arrives.
        let ended = self.recorder.stop();
        self.captured(
            RecorderResult {
                stopped: true,
                shortcut: None,
                ended,
            },
            cx,
        );
    }

    /// Tells the window's own key handling what the recorder is doing.
    fn mirror_capture(&self, cx: &mut Context<Self>) {
        let _ = cx;
        #[cfg(windows)]
        self.keys
            .set_capture(self.recorder.mode(), self.recorder.token());
    }

    pub(crate) fn capturing(&self) -> bool {
        self.recorder.mode() != RecorderMode::Idle
    }

    // Settings

    pub(crate) fn microphone_labels(&self) -> Options {
        let mut labels = options(["System default"]);
        labels.extend(
            self.microphones
                .iter()
                .map(|name| SharedString::from(name.clone())),
        );
        labels
    }

    pub(crate) fn effort_labels(&self) -> Options {
        let mut labels = options(["Model default"]);
        labels.extend(
            self.efforts
                .iter()
                .map(|effort| SharedString::new_static(rules::reasoning_label(*effort))),
        );
        labels
    }

    pub(crate) fn load_settings(
        &mut self,
        settings: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft = settings.clone();
        for key in [&self.azure_key, &self.openrouter_key] {
            key.update(cx, |key, cx| key.set_value("", window, cx));
        }
        // A saved microphone that is not connected stays selected.
        if let Some(microphone) = &settings.microphone {
            if !self.microphones.contains(microphone) {
                self.microphones.push(microphone.clone());
            }
        }
        let labels = self.microphone_labels();
        let index = settings
            .microphone
            .as_ref()
            .and_then(|name| self.microphones.iter().position(|m| m == name))
            .map_or(0, |index| index + 1);
        self.microphone.update(cx, |state, cx| {
            state.set_items(labels, window, cx);
            state.set_selected_index(Some(IndexPath::new(index)), window, cx);
        });
        self.choice
            .cleanup(&mut self.draft, settings.cleanup_engine);
        let model = settings.cleanup_model.clone();
        self.model
            .update(cx, |state, cx| state.set_value(model, window, cx));
        let styling = rules::STYLINGS
            .iter()
            .position(|styling| *styling == settings.cleanup_styling)
            .unwrap_or(0);
        pick(&self.styling, styling, window, cx);
        let unload = rules::UNLOAD_CHOICES
            .iter()
            .position(|choice| choice.seconds == settings.cleanup_unload_seconds)
            .unwrap_or(0);
        pick(&self.unload, unload, window, cx);
        self.show_efforts(window, cx);
        self.show_cleanup(window, cx);
    }

    /// Streaming, transcription and cleanup can each change the others.
    pub(crate) fn show_cleanup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = MODELS
            .iter()
            .position(|(model, _)| *model == self.draft.transcription_model)
            .unwrap_or(0);
        pick(&self.transcription, model, window, cx);
        let engine = ENGINES
            .iter()
            .position(|(engine, _)| *engine == self.draft.cleanup_engine)
            .unwrap_or(0);
        pick(&self.engine, engine, window, cx);
        cx.notify();
    }

    pub(crate) fn set_streaming(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.choice.streaming(&mut self.draft, on);
        self.show_cleanup(window, cx);
    }

    pub(crate) fn show_efforts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.efforts = rules::available_efforts(&self.models, &self.draft.cleanup_model);
        let effective = rules::effective_effort(&self.draft, &self.models);
        let index = effective
            .and_then(|effort| self.efforts.iter().position(|e| *e == effort))
            .map_or(0, |index| index + 1);
        let labels = self.effort_labels();
        self.effort.update(cx, |state, cx| {
            state.set_items(labels, window, cx);
            state.set_selected_index(Some(IndexPath::new(index)), window, cx);
        });
        cx.notify();
    }

    pub(crate) fn load_models(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.loading_models = true;
        self.models_failed = false;
        let app = self.app.clone();
        self.run_or(
            window,
            cx,
            async move { app.cleanup_models().await },
            |this, models, window, cx| {
                this.models = models;
                this.loading_models = false;
                this.show_efforts(window, cx);
            },
            |this, _, _, cx| {
                this.loading_models = false;
                this.models_failed = true;
                cx.notify();
            },
        );
        cx.notify();
    }

    pub(crate) fn choose_model(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if id != self.draft.cleanup_model {
            self.draft.cleanup_reasoning_effort = None;
        }
        self.draft.cleanup_model = id.clone();
        self.model
            .update(cx, |state, cx| state.set_value(id, window, cx));
        self.models_open = false;
        self.show_efforts(window, cx);
    }

    pub(crate) fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.capturing() {
            return;
        }
        let mut settings = self.draft.clone();
        settings.cleanup_reasoning_effort = rules::effective_effort(&self.draft, &self.models);
        let entered = |state: &Entity<InputState>, cx: &App| {
            let value = state.read(cx).value().trim().to_string();
            (!value.is_empty()).then_some(value)
        };
        let key = entered(&self.openrouter_key, cx);
        let azure_key = entered(&self.azure_key, cx);
        let (has_key, has_azure) = (key.is_some(), azure_key.is_some());
        self.saving = true;
        self.command_error = None;
        self.refresh_toast(cx);
        let app = self.app.clone();
        self.run_or(
            window,
            cx,
            async move { app.save_settings(settings, key, azure_key).await },
            move |this, saved, window, cx| {
                this.saving = false;
                this.has_azure_key |= has_azure;
                this.has_openrouter_key |= has_key;
                this.load_settings(saved, window, cx);
                #[cfg(windows)]
                this.keys.set_binding(this.app.shortcut_binding());
                this.notice_saved(window, cx);
            },
            |this, message, _, cx| {
                this.saving = false;
                this.fail(message, cx);
            },
        );
        cx.notify();
    }

    pub(crate) fn update_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app = self.app.clone();
        if self.update.phase == "available" {
            self.run(
                window,
                cx,
                async move { app.install_update().await },
                |_, _, _, cx| cx.quit(),
            );
        } else {
            self.run(
                window,
                cx,
                async move { app.check_update().await },
                |this, view, _, cx| this.update_view(view, cx),
            );
        }
    }

    pub(crate) fn download(
        &mut self,
        part: s1::Part,
        cancel: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let app = self.app.clone();
        self.run(
            window,
            cx,
            blocking(move || {
                if cancel {
                    app.cancel_s1_download(part)
                } else {
                    app.download_s1(part)
                }
                Ok(())
            }),
            |_, _, _, _| {},
        );
    }

    // History

    pub(crate) fn refresh(&mut self, window: &Window, cx: &mut Context<Self>) {
        let app = self.app.clone();
        self.run_or(
            window,
            cx,
            blocking(move || app.history()),
            |this, entries, window, cx| {
                this.entries = entries;
                this.history_failed = false;
                this.show_entries(window, cx);
            },
            |this, _, _, cx| {
                this.history_failed = true;
                cx.notify();
            },
        );
    }

    /// The entries the search matches, as indices into `entries`.
    pub(crate) fn shown(&self, cx: &App) -> Vec<usize> {
        let search = self.search.read(cx).value();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| rules::matches(&entry.text, &search))
            .map(|(index, _)| index)
            .collect()
    }

    pub(crate) fn show_entries(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self
            .entries
            .iter()
            .any(|entry| Some(&entry.id) == self.selected.as_ref())
        {
            self.selected = self.entries.first().map(|entry| entry.id.clone());
        }
        let selected = self.selected.clone();
        self.select(selected, window, cx);
    }

    pub(crate) fn select(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| Some(&entry.id) == id.as_ref())
            .cloned()
        else {
            self.selected = None;
            cx.notify();
            return;
        };
        let switched = self.selected != id || self.shown_text != entry.text;
        self.selected = id;
        if switched {
            self.shown_text = entry.text.clone();
            self.text
                .update(cx, |text, cx| text.set_value(entry.text, window, cx));
        }
        cx.notify();
    }

    pub(crate) fn pick_entry(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.save_text(window, cx);
        self.select(Some(id), window, cx);
    }

    pub(crate) fn text_value(&self, cx: &App) -> String {
        self.text.read(cx).value().to_string()
    }

    pub(crate) fn save_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let value = self.text_value(cx);
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return;
        };
        if entry.text == value {
            return;
        }
        entry.text = value.clone();
        self.shown_text = value.clone();
        let app = self.app.clone();
        self.run(
            window,
            cx,
            blocking(move || app.edit_entry(&id, &value)),
            |this, _, window, cx| this.refresh(window, cx),
        );
    }

    pub(crate) fn copy_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let value = self.text_value(cx);
        let edited = self
            .entries
            .iter()
            .any(|entry| entry.id == id && entry.text != value);
        let app = self.app.clone();
        self.run(
            window,
            cx,
            async move {
                if edited {
                    app.edit_entry(&id, &value)?;
                }
                app.copy_entry(&id).await
            },
            |this, _, window, cx| {
                this.copied = Some(cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(1500))
                        .await;
                    this.update(cx, |this, cx| {
                        this.copied = None;
                        cx.notify();
                    })
                    .ok();
                }));
                this.refresh(window, cx);
            },
        );
    }

    pub(crate) fn delete_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let app = self.app.clone();
        self.run(
            window,
            cx,
            blocking(move || app.delete_entry(&id)),
            |this, _, window, cx| this.refresh(window, cx),
        );
    }

    fn top_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme::tokens(cx);
        let s = &self.session;
        let recording = s.phase == Phase::Recording;
        let busy = rules::working(s.phase);
        let done = rules::finished(s);
        let status = rules::status_label(s);
        let record_label: SharedString = if recording {
            "Stop".into()
        } else if busy {
            status.unwrap_or_default().into()
        } else {
            "Record".into()
        };
        let record_icon = if busy && done {
            Some(controls::CHECK)
        } else if busy {
            None
        } else if recording {
            Some(controls::STOP)
        } else {
            Some(controls::MIC)
        };
        let clock =
            recording.then(|| rules::clock(s.started_at, chrono::Utc::now().timestamp_millis()));
        let shell = self.shell.clone();
        let tab = self.tab;
        let entity = cx.entity();
        let to_history = entity.clone();
        let to_settings = entity.clone();
        let _ = window;
        TitleBar::new()
            .h(px(52.))
            .pl(px(18.))
            .on_close_window(move |_, window, cx| {
                if let Some(shell) = shell.upgrade() {
                    shell.update(cx, |shell, _| shell.main_closed());
                }
                window.remove_window();
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap(px(18.))
                    .pr(px(12.))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(px(17.))
                            .text_color(t.ink)
                            .child("Transcribe"),
                    )
                    .child(
                        tabs()
                            .tab("History", tab == Tab::History, move |_, cx| {
                                to_history.update(cx, |this, cx| this.show_tab(Tab::History, cx))
                            })
                            .tab("Settings", tab == Tab::Settings, move |_, cx| {
                                to_settings.update(cx, |this, cx| this.show_tab(Tab::Settings, cx))
                            }),
                    )
                    .child(div().flex_1())
                    .when_some(clock, |this, clock| {
                        this.child(
                            div()
                                .text_size(px(14.))
                                .font_weight(FontWeight::MEDIUM)
                                .font_features(tabular())
                                .text_color(t.ink)
                                .child(clock),
                        )
                    })
                    .when(recording || (busy && !done), |this| {
                        this.child(
                            button("cancel", Tone::Ghost)
                                .icon(controls::CLOSE)
                                .tooltip("Cancel recording")
                                .on_click(cx.listener(|this, _, _, _| this.app.cancel())),
                        )
                    })
                    .child(
                        button("import", Tone::Outline)
                            .icon(controls::IMPORT)
                            .label("Import audio")
                            .disabled(busy || recording || !self.has_azure_key)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.import_audio(window, cx)),
                            ),
                    )
                    .child(
                        button("record", Tone::Primary)
                            .map(|this| match record_icon {
                                Some(path) => this.icon(path),
                                None => this.spinning(true),
                            })
                            .label(record_label)
                            .disabled(busy || !self.has_azure_key)
                            .on_click(cx.listener(|this, _, window, cx| this.record(window, cx))),
                    ),
            )
    }

    fn progress(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let t = theme::tokens(cx);
        let working = rules::processing(&self.session) && self.session.error.is_none();
        if !working {
            return None;
        }
        let done = rules::finished(&self.session);
        let bar = div().h(px(2.)).w_full().bg(t.ink);
        Some(
            div()
                .h(px(2.))
                .w_full()
                .bg(t.paper_3)
                .child(if done || cx.reduce_motion() {
                    bar.into_any_element()
                } else {
                    bar.with_animation(
                        "progress",
                        Animation::new(Duration::from_millis(1400)).repeat(),
                        |bar, delta| bar.opacity(0.25 + 0.75 * (1. - (delta * 2. - 1.).abs())),
                    )
                    .into_any_element()
                }),
        )
    }

    fn toasts(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let t = theme::tokens(cx);
        let (message, error) = match (&self.toast, &self.saved) {
            (Some((message, _)), _) => (message.clone(), true),
            (None, Some(_)) => ("Saved".to_string(), false),
            (None, None) => return None,
        };
        Some(
            div()
                .absolute()
                .bottom(px(20.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .max_w(px(560.))
                        .min_h(px(44.))
                        .pl(px(16.))
                        .pr(px(6.))
                        .py(px(5.))
                        .rounded(px(12.))
                        .bg(t.paper)
                        .border_1()
                        .border_color(t.rule)
                        .shadow(t.float_shadow())
                        .when(error, |this| {
                            this.child(icon(controls::ALERT, px(16.), t.ink))
                        })
                        .when(!error, |this| {
                            this.child(icon(controls::CHECK, px(16.), t.ink))
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(14.))
                                .text_color(t.ink)
                                .child(message),
                        )
                        .child(
                            button("toast-dismiss", Tone::Ghost)
                                .icon(controls::CLOSE)
                                .small()
                                .tooltip("Dismiss")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if this.toast.is_some() {
                                        this.dismiss_toast(cx);
                                    } else {
                                        this.saved = None;
                                        cx.notify();
                                    }
                                })),
                        ),
                ),
        )
    }
}

impl Focusable for MainWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let body = match self.tab {
            Tab::History => self.history(window, cx).into_any_element(),
            Tab::Settings => self.settings(window, cx).into_any_element(),
        };
        let top_bar = self.top_bar(window, cx);
        let progress = self.progress(cx);
        let toasts = self.toasts(cx);
        div()
            .id("main")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .relative()
            .bg(t.paper)
            .text_color(t.ink)
            .font_family(theme::BODY)
            .text_size(theme::TEXT)
            // Keys being recorded as a shortcut must not also operate the window.
            .capture_key_down(cx.listener(|this, _, _, cx| {
                if this.capturing() {
                    cx.stop_propagation();
                }
            }))
            .child(top_bar)
            .child(div().h(px(2.)).w_full().children(progress))
            .child(div().flex_1().min_h_0().child(body))
            .children(toasts)
    }
}
