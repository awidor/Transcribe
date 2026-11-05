//! Owns the windows and routes what the core reports to them.
use crate::{
    main_window::{MainWindow, Parts, Tab},
    pill::{self, Action, FromHelper, Model, Placement},
};
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Context, Entity, Task, WindowBounds,
    WindowDecorations, WindowOptions, px, size,
};
use gpui_component::{Root, TitleBar};
#[cfg(target_os = "linux")]
use std::time::Duration;
use std::{
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};
use tokio::runtime::Handle;
use transcribe_core::session::{App as Core, Event, Phase, Session};

/// What reaches the UI thread.
pub enum UiEvent {
    Core(Event),
    /// A second launch or the tray asks for History.
    Show,
    Quit,
    /// Windows: the window's own key handling could not record a key.
    #[cfg_attr(not(windows), allow(dead_code))]
    CaptureFailed(String),
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Helper(FromHelper),
}

pub struct Shell {
    pub app: Core,
    pub runtime: Handle,
    pub level: Arc<AtomicU32>,
    pub focused: Arc<AtomicBool>,
    main: Option<(AnyWindowHandle, Entity<MainWindow>)>,
    placement: Placement,
    pill: Option<pill::Open>,
    model: Option<Model>,
    #[cfg(target_os = "linux")]
    helper: Option<crate::helper::Helper>,
    forwarding: Option<Task<()>>,
    #[cfg(windows)]
    pub keys: Arc<crate::windows::KeyRouter>,
    #[cfg(windows)]
    _reshow: Option<Task<()>>,
    #[cfg(target_os = "linux")]
    events: futures::channel::mpsc::UnboundedSender<UiEvent>,
}

impl Shell {
    pub fn new(
        app: Core,
        runtime: Handle,
        level: Arc<AtomicU32>,
        focused: Arc<AtomicBool>,
        events: futures::channel::mpsc::UnboundedSender<UiEvent>,
        cx: &mut Context<Self>,
    ) -> Self {
        let placement = if cfg!(target_os = "linux") && transcribe_core::is_wayland() {
            Placement::Layer
        } else {
            Placement::Popup
        };
        #[cfg(windows)]
        let keys = Arc::new(crate::windows::KeyRouter::new(app.clone(), events.clone()));
        let _ = cx;
        Self {
            app,
            runtime,
            level,
            focused,
            main: None,
            placement,
            pill: None,
            model: None,
            #[cfg(target_os = "linux")]
            helper: None,
            forwarding: None,
            #[cfg(windows)]
            keys,
            #[cfg(windows)]
            _reshow: Some(crate::windows::reshow(cx)),
            #[cfg(target_os = "linux")]
            events,
        }
    }

    pub fn handle(&mut self, event: UiEvent, cx: &mut Context<Self>) {
        match event {
            UiEvent::Core(Event::Session(session)) => {
                // Levels only arrive while recording; an old one must not linger.
                if session.phase != Phase::Recording {
                    self.level.store(0f32.to_bits(), Ordering::Relaxed);
                }
                let shown = session.clone();
                self.with_main(cx, move |main, window, cx| {
                    main.set_session(shown, window, cx)
                });
                self.pill_session(&session, cx);
            }
            UiEvent::Core(Event::History) => {
                self.with_main(cx, |main, window, cx| main.refresh(window, cx));
            }
            UiEvent::Core(Event::OpenHistory) | UiEvent::Show => self.show(Tab::History, cx),
            UiEvent::Core(Event::S1(status)) => {
                self.with_main(cx, move |main, _, cx| main.set_s1(status, cx));
            }
            UiEvent::Core(Event::Capture(event)) => {
                self.with_main(cx, move |main, _, cx| main.capture(event, cx));
            }
            UiEvent::CaptureFailed(message) => {
                self.with_main(cx, move |main, _, cx| main.capture_failed(message, cx));
            }
            UiEvent::Core(Event::Update(view)) => {
                self.with_main(cx, move |main, _, cx| main.update_view(view, cx));
            }
            UiEvent::Core(Event::ShowWidget) => {
                if let Some(session) = self.app.widget_to_show(false) {
                    let result = self.pill_show(&session, cx);
                    // The helper reports its own outcome once it has tried.
                    if self.placement != Placement::Helper || result.is_err() {
                        self.app
                            .widget_shown(result.err().map(|error| error.to_string()));
                    }
                }
            }
            UiEvent::Core(Event::HideWidget) => self.pill_hide(cx),
            UiEvent::Helper(FromHelper::Shown(error)) => self.app.widget_shown(error),
            UiEvent::Helper(FromHelper::Action(action)) => self.pill_action(action),
            UiEvent::Quit => {
                self.app.shutdown();
                cx.quit();
            }
        }
    }

    fn with_main(
        &self,
        cx: &mut App,
        f: impl FnOnce(&mut MainWindow, &mut gpui::Window, &mut Context<MainWindow>) + 'static,
    ) {
        if let Some((handle, view)) = &self.main {
            let view = view.clone();
            handle
                .update(cx, move |_, window, cx| {
                    view.update(cx, |main, cx| f(main, window, cx))
                })
                .ok();
        }
    }

    pub fn main_closed(&mut self) {
        self.main = None;
        self.focused.store(false, Ordering::Relaxed);
    }

    /// Opens the main window on `tab`, or brings it forward.
    pub fn show(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if let Some((handle, view)) = &self.main {
            let view = view.clone();
            let shown = handle.update(cx, |_, window, cx| {
                view.update(cx, |main, cx| main.show_tab(tab, cx));
                window.activate_window();
            });
            if shown.is_ok() {
                return;
            }
            self.main = None;
        }
        let parts = Parts {
            shell: cx.entity().downgrade(),
            app: self.app.clone(),
            runtime: self.runtime.clone(),
            focused: self.focused.clone(),
            #[cfg(windows)]
            keys: self.keys.clone(),
        };
        let bounds = Bounds::centered(None, size(px(1000.), px(680.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(760.), px(520.))),
            app_id: Some("app.transcribe.desktop".into()),
            window_decorations: Some(WindowDecorations::Client),
            ..TitleBar::window_options()
        };
        let mut created = None;
        let opened = cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| MainWindow::new(parts, tab, window, cx));
            created = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        });
        let handle = match opened {
            Ok(handle) => handle,
            Err(error) => {
                eprintln!("Window unavailable: {error}");
                return;
            }
        };
        let weak = cx.entity().downgrade();
        handle
            .update(cx, |_, window, cx| {
                window.on_window_should_close(cx, move |_, cx| {
                    weak.update(cx, |shell, _| shell.main_closed()).ok();
                    true
                });
                window.activate_window();
                #[cfg(windows)]
                crate::windows::route_keys(window, self.keys.clone());
            })
            .ok();
        if let Some(view) = created {
            self.main = Some((handle.into(), view));
        }
    }

    // The pill

    fn act(&self, cx: &Context<Self>) -> Rc<dyn Fn(Action, &mut App)> {
        let shell = cx.entity().downgrade();
        Rc::new(move |action, cx| {
            shell.update(cx, |shell, _| shell.pill_action(action)).ok();
        })
    }

    fn pill_action(&mut self, action: Action) {
        let app = self.app.clone();
        match action {
            Action::Stop => {
                self.runtime.spawn(async move {
                    if let Err(error) = app.toggle(false).await {
                        eprintln!("{error}");
                    }
                });
            }
            Action::Cancel => app.cancel(),
            Action::History => app.open_history(),
            Action::Copy => {
                let Some(text) = self
                    .model
                    .as_ref()
                    .and_then(|model| model.transcript.clone())
                else {
                    return;
                };
                self.runtime.spawn(async move {
                    match app.copy_text(text).await {
                        Ok(()) => app.cancel(),
                        Err(error) => eprintln!("{error}"),
                    }
                });
            }
        }
    }

    fn pill_show(&mut self, session: &Session, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let model = Model::new(session);
        self.present(model, true, cx)
    }

    fn pill_session(&mut self, session: &Session, cx: &mut Context<Self>) {
        let Some(shown) = &self.model else {
            return;
        };
        let model = if session.phase == Phase::Idle {
            // The pill keeps showing its last session while it closes.
            Model {
                closing: true,
                ..shown.clone()
            }
        } else {
            Model::new(session)
        };
        if let Err(error) = self.present(model, false, cx) {
            eprintln!("Recording widget: {error}");
        }
    }

    fn present(
        &mut self,
        model: Model,
        opening: bool,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        self.model = Some(model.clone());
        if self.placement == Placement::Helper {
            #[cfg(target_os = "linux")]
            {
                if self.helper.as_ref().is_none_or(|helper| !helper.alive()) {
                    self.helper = Some(crate::helper::Helper::spawn(self.events.clone())?);
                }
                if let Some(helper) = &self.helper {
                    helper.send(&pill::ToHelper::Show(model));
                }
                self.forward_level(cx);
            }
            return Ok(());
        }
        if let Some(open) = &self.pill {
            let wanted = model.window_size();
            if open.size == wanted || self.placement == Placement::Layer {
                let view = open.view.clone();
                let resize = open.size != wanted;
                let updated = open.handle.update(cx, |_, window, cx| {
                    if resize {
                        window.resize(wanted);
                    }
                    view.update(cx, |view, cx| view.set(model.clone(), cx));
                });
                if updated.is_ok() {
                    if let Some(open) = &mut self.pill {
                        open.size = wanted;
                    }
                    return Ok(());
                }
            }
            self.close_pill(cx);
        }
        if !opening && model.closing {
            return Ok(());
        }
        match pill::open(
            self.placement,
            model.clone(),
            self.level.clone(),
            self.act(cx),
            cx,
        ) {
            Ok(open) => {
                self.pill = Some(open);
                Ok(())
            }
            // Without layer shell (GNOME) the pill opens on XWayland instead.
            Err(error) if self.placement == Placement::Layer => {
                eprintln!("Layer shell unavailable: {error}");
                self.placement = Placement::Helper;
                self.present(model, opening, cx)
            }
            Err(error) => Err(error),
        }
    }

    fn close_pill(&mut self, cx: &mut App) {
        if let Some(open) = self.pill.take() {
            open.handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        }
    }

    fn pill_hide(&mut self, cx: &mut Context<Self>) {
        self.model = None;
        self.forwarding = None;
        self.close_pill(cx);
        #[cfg(target_os = "linux")]
        if let Some(helper) = &self.helper {
            helper.send(&pill::ToHelper::Hide);
        }
    }

    /// Sends the voice level to the helper while it shows a recording.
    #[cfg(target_os = "linux")]
    fn forward_level(&mut self, cx: &mut Context<Self>) {
        if self.forwarding.is_some() {
            return;
        }
        self.forwarding = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                let sent = this.update(cx, |shell, _| {
                    let recording = shell.model.as_ref().is_some_and(|model| model.recording);
                    if recording {
                        let level = f32::from_bits(shell.level.load(Ordering::Relaxed));
                        if let Some(helper) = &shell.helper {
                            helper.send(&pill::ToHelper::Level(level));
                        }
                    }
                    shell.model.is_some()
                });
                if !matches!(sent, Ok(true)) {
                    return;
                }
            }
        }));
    }

    /// Windows: puts a live recording's pill back after something hid it.
    #[cfg(windows)]
    pub fn reassert_pill(&mut self, cx: &mut App) {
        if self.app.widget_to_show(true).is_none() {
            return;
        }
        if let Some(open) = &self.pill {
            open.handle
                .update(cx, |_, window, _| crate::windows::float(window))
                .ok();
        }
    }
}
