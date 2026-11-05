//! The recording pill: a black capsule at the bottom of the screen whose white
//! bars move with the voice, then the transcript it could not paste.
use crate::{
    controls::{self, Tone, button, icon, spinner, tabular},
    theme::{self, Tokens},
};
use gpui::{
    Animation, AnimationExt, AnyWindowHandle, App, AppContext, Bounds, Context, Entity, FontWeight,
    IntoElement, ParentElement, Pixels, PlatformDisplay, Render, Size, Styled, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, canvas, div,
    ease_out_quint, fill, point, prelude::FluentBuilder, px, size,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use transcribe_core::{
    rules::{self, Loudness, WidgetMode},
    session::{Phase, Session},
};

/// Room around the part for its shadow.
const MARGIN: Pixels = px(12.);
const WIDTH: Pixels = px(380.);
/// Without words, the pill holds only the meter or status and its buttons.
const COMPACT: Pixels = px(220.);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    Session,
    Error,
    Card,
}

/// What the pill shows, from a session. It crosses to the helper process as JSON.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub mode: Mode,
    pub recording: bool,
    pub done: bool,
    pub started_at: Option<i64>,
    pub status: String,
    pub words: Option<(String, String)>,
    pub error: Option<String>,
    pub transcript: Option<String>,
    pub closing: bool,
}

impl Model {
    pub fn new(session: &Session) -> Self {
        Self {
            mode: match rules::widget_mode(session) {
                WidgetMode::Session => Mode::Session,
                WidgetMode::Error => Mode::Error,
                WidgetMode::Card => Mode::Card,
            },
            recording: session.phase == Phase::Recording,
            done: rules::finished(session),
            started_at: session.started_at,
            status: rules::status_label(session).unwrap_or("Starting").into(),
            words: rules::spoken(&session.words).map(|words| (words.text, words.interim)),
            error: session.error.clone(),
            transcript: session.transcript.clone(),
            closing: false,
        }
    }

    fn height(&self) -> Pixels {
        match self.mode {
            Mode::Card => px(72.),
            _ => px(44.),
        }
    }

    fn width(&self) -> Pixels {
        if self.mode == Mode::Session && self.words.is_none() {
            COMPACT
        } else {
            WIDTH
        }
    }

    pub fn window_size(&self) -> Size<Pixels> {
        size(self.width() + MARGIN * 2., self.height() + MARGIN * 2.)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Stop,
    Cancel,
    History,
    Copy,
}

/// Parent to helper.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum ToHelper {
    Show(Model),
    Hide,
    Level(f32),
}

/// Helper to parent.
#[derive(Debug, Serialize, Deserialize)]
pub enum FromHelper {
    Shown(Option<String>),
    Action(Action),
}

/// How many recent levels the bars show.
const BARS: usize = 9;
/// How often a new level enters at the right.
const STEP: Duration = Duration::from_millis(70);

pub struct PillView {
    model: Model,
    level: Arc<AtomicU32>,
    loudness: Loudness,
    listening: bool,
    last_frame: Option<Instant>,
    since_step: Duration,
    bars: VecDeque<f32>,
    act: Rc<dyn Fn(Action, &mut App)>,
}

impl PillView {
    pub fn new(model: Model, level: Arc<AtomicU32>, act: Rc<dyn Fn(Action, &mut App)>) -> Self {
        Self {
            model,
            level,
            loudness: Loudness::default(),
            listening: false,
            last_frame: None,
            since_step: Duration::ZERO,
            bars: VecDeque::from(vec![0.; BARS]),
            act,
        }
    }

    pub fn set(&mut self, model: Model, cx: &mut Context<Self>) {
        if self.model != model {
            self.model = model;
            cx.notify();
        }
    }

    /// Advances the meter by the time since the last frame.
    fn tick(&mut self) {
        let now = Instant::now();
        let dt = self.last_frame.map_or(Duration::ZERO, |last| {
            (now - last).min(Duration::from_millis(100))
        });
        self.last_frame = Some(now);
        let listening = !self.model.closing && self.model.recording;
        // Every recording learns its room afresh.
        if listening && !self.listening {
            self.loudness = Loudness::default();
            self.bars = VecDeque::from(vec![0.; BARS]);
        }
        self.listening = listening;
        if !listening {
            return;
        }
        let level = f32::from_bits(self.level.load(Ordering::Relaxed)) as f64;
        let loudness = self.loudness.update(level, dt.as_secs_f64()) as f32;
        self.since_step += dt;
        if self.since_step >= STEP {
            self.since_step = Duration::ZERO;
            self.bars.pop_front();
            self.bars.push_back(loudness);
        } else if let Some(last) = self.bars.back_mut() {
            *last = last.max(loudness);
        }
    }

    fn action(
        &self,
        action: Action,
    ) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
        let act = self.act.clone();
        move |_, _, cx| act(action, cx)
    }

    fn row(&self) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(16.))
            .pr(px(6.))
            .size_full()
    }

    fn session(&mut self, t: Tokens, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        self.tick();
        let m = &self.model;
        let recording = m.recording;
        if recording && !m.closing {
            window.request_animation_frame();
        }
        let lead = if recording {
            meter(self.bars.iter().copied().collect(), t).into_any_element()
        } else if m.done {
            icon(controls::CHECK, px(16.), t.ink).into_any_element()
        } else {
            spinner("pill-spin", px(16.), t.ink, cx)
        };
        let status = if recording {
            rules::clock(m.started_at, chrono::Utc::now().timestamp_millis())
        } else {
            m.status.clone()
        };
        let words = m.words.clone();
        let done = m.done;
        self.row()
            .child(lead)
            .child(
                div()
                    .flex_none()
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .font_features(tabular())
                    .text_color(if recording { t.muted } else { t.ink })
                    .child(status),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis_start()
                    .text_size(px(13.))
                    .text_color(t.ink)
                    .when_some(words, |this, (text, interim)| {
                        this.child(
                            gpui::StyledText::new(format!("{text}{interim}")).with_highlights([(
                                text.len()..text.len() + interim.len(),
                                gpui::HighlightStyle {
                                    color: Some(t.muted),
                                    ..Default::default()
                                },
                            )]),
                        )
                    }),
            )
            .when(recording, |this| {
                this.child(
                    button("pill-stop", Tone::Primary)
                        .icon(controls::STOP)
                        .small()
                        .tooltip("Stop")
                        .palette(t)
                        .on_click(self.action(Action::Stop)),
                )
            })
            .when(!done, |this| {
                this.child(
                    button("pill-cancel", Tone::Ghost)
                        .icon(controls::CLOSE)
                        .small()
                        .tooltip("Cancel")
                        .palette(t)
                        .on_click(self.action(Action::Cancel)),
                )
            })
    }

    fn error(&self, t: Tokens) -> gpui::Div {
        self.row()
            .child(icon(controls::ALERT, px(16.), t.ink))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(13.))
                    .text_color(t.ink)
                    .child(self.model.error.clone().unwrap_or_else(|| "Error".into())),
            )
            .child(
                button("pill-history", Tone::Outline)
                    .label("History")
                    .small()
                    .palette(t)
                    .on_click(self.action(Action::History)),
            )
            .child(
                button("pill-dismiss", Tone::Ghost)
                    .icon(controls::CLOSE)
                    .small()
                    .tooltip("Dismiss")
                    .palette(t)
                    .on_click(self.action(Action::Cancel)),
            )
    }

    fn card(&self, t: Tokens) -> gpui::Div {
        self.row()
            .pl(px(18.))
            .pr(px(10.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .line_clamp(2)
                    .text_size(px(13.))
                    .line_height(px(19.))
                    .text_color(t.ink)
                    .child(self.model.transcript.clone().unwrap_or_default()),
            )
            .child(
                button("pill-copy", Tone::Primary)
                    .icon(controls::COPY)
                    .label("Copy")
                    .small()
                    .palette(t)
                    .on_click(self.action(Action::Copy)),
            )
            .child(
                button("pill-dismiss", Tone::Ghost)
                    .icon(controls::CLOSE)
                    .small()
                    .tooltip("Dismiss")
                    .palette(t)
                    .on_click(self.action(Action::Cancel)),
            )
    }
}

/// White bars, newest on the right, painted rather than laid out each frame.
fn meter(bars: Vec<f32>, t: Tokens) -> gpui::Div {
    const WIDTH: f32 = 3.;
    const GAP: f32 = 3.;
    let span = BARS as f32 * (WIDTH + GAP) - GAP;
    div().flex_none().w(px(span)).h(px(20.)).child(
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let full = bounds.size.height;
                for (index, level) in bars.iter().enumerate() {
                    let height = px(4.).max(full * level.clamp(0., 1.));
                    let x = bounds.origin.x + px(index as f32 * (WIDTH + GAP));
                    let y = bounds.origin.y + (full - height) / 2.;
                    let bar = Bounds {
                        origin: point(x, y),
                        size: size(px(WIDTH), height),
                    };
                    let color = if *level > 0.02 { t.ink } else { t.muted };
                    window.paint_quad(fill(bar, color).corner_radii(px(WIDTH / 2.)));
                }
            },
        )
        .size_full(),
    )
}

impl Render for PillView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::pill();
        let closing = self.model.closing;
        let card = self.model.mode == Mode::Card;
        let content = match self.model.mode {
            Mode::Session => self.session(t, window, cx),
            Mode::Error => self.error(t),
            Mode::Card => self.card(t),
        };
        let part = div()
            .flex()
            .w(self.model.width())
            .h(self.model.height())
            .rounded(if card {
                px(20.)
            } else {
                self.model.height() / 2.
            })
            .bg(t.paper)
            .font_family(theme::BODY)
            .text_color(t.ink)
            .child(content);
        let part = if cx.reduce_motion() {
            part.opacity(if closing { 0. } else { 1. })
                .into_any_element()
        } else if closing {
            part.with_animation(
                "pill-close",
                Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
                |part, delta| part.opacity(1. - delta),
            )
            .into_any_element()
        } else {
            part.with_animation(
                "pill-open",
                Animation::new(Duration::from_millis(200)).with_easing(ease_out_quint()),
                |part, delta| part.opacity(delta),
            )
            .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .items_end()
            .justify_center()
            .p(MARGIN)
            .child(part)
    }
}

/// Where the pill goes on this desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// A wlr layer surface anchored to the bottom (KDE, wlroots).
    Layer,
    /// An override-redirect or topmost tool window at a computed position.
    Popup,
    /// A child process on XWayland (GNOME), which honours position and stacking.
    Helper,
}

/// The primary display. Wayland names none, so there the desktop's own
/// ranking decides, or else the display at the origin.
fn display(cx: &App) -> Option<Rc<dyn PlatformDisplay>> {
    if let Some(primary) = cx.primary_display() {
        return Some(primary);
    }
    let displays = cx.displays();
    #[cfg(target_os = "linux")]
    if let Some(name) = transcribe_core::display::primary_output() {
        // GPUI identifies a Wayland output by a UUID of its name.
        let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes());
        if let Some(display) = displays
            .iter()
            .find(|display| display.uuid().is_ok_and(|uuid| uuid == id))
        {
            return Some(display.clone());
        }
    }
    let origin = point(px(0.), px(0.));
    displays
        .iter()
        .find(|display| display.bounds().contains(&origin))
        .cloned()
        .or_else(|| displays.into_iter().next())
}

pub fn options(placement: Placement, model: &Model, cx: &App) -> WindowOptions {
    let window = model.window_size();
    let display = display(cx);
    let (kind, origin) = match placement {
        #[cfg(target_os = "linux")]
        Placement::Layer => (
            WindowKind::LayerShell(gpui::layer_shell::LayerShellOptions {
                namespace: "transcribe-widget".into(),
                layer: gpui::layer_shell::Layer::Overlay,
                anchor: gpui::layer_shell::Anchor::BOTTOM,
                exclusive_zone: None,
                exclusive_edge: None,
                margin: Some((px(0.), px(0.), px(16.) - MARGIN, px(0.))),
                keyboard_interactivity: gpui::layer_shell::KeyboardInteractivity::None,
            }),
            point(px(0.), px(0.)),
        ),
        _ => {
            let area = display.as_ref().map(|display| {
                if cfg!(windows) {
                    display.visible_bounds()
                } else {
                    display.bounds()
                }
            });
            let gap = if cfg!(windows) { px(24.) } else { px(64.) } - MARGIN;
            let origin = area.map_or(point(px(0.), px(0.)), |area| {
                point(
                    area.origin.x + (area.size.width - window.width) / 2.,
                    area.origin.y + area.size.height - gap - window.height,
                )
            });
            (WindowKind::PopUp, origin)
        }
    };
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin,
            size: window,
        })),
        titlebar: None,
        focus: false,
        // Windows shows it without activating once it cannot take focus.
        show: !cfg!(windows),
        kind,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: display.map(|display| display.id()),
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("app.transcribe.desktop".into()),
        ..Default::default()
    }
}

/// An open pill window and the view in it.
pub struct Open {
    pub handle: AnyWindowHandle,
    pub view: Entity<PillView>,
    pub size: Size<Pixels>,
}

pub fn open(
    placement: Placement,
    model: Model,
    level: Arc<AtomicU32>,
    act: Rc<dyn Fn(Action, &mut App)>,
    cx: &mut App,
) -> anyhow::Result<Open> {
    let size = model.window_size();
    let options = options(placement, &model, cx);
    let mut created = None;
    let handle = cx.open_window(options, |window, cx| {
        let view = cx.new(|_| PillView::new(model, level, act));
        created = Some(view.clone());
        let _ = window;
        view
    })?;
    #[cfg(windows)]
    handle
        .update(cx, |_, window, _| crate::windows::float(window))
        .ok();
    Ok(Open {
        handle: handle.into(),
        view: created.expect("pill view"),
        size,
    })
}
