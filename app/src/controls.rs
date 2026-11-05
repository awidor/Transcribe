//! Flat monochrome controls drawn from the theme's tokens.
use crate::theme::{self, Tokens};
use gpui::{
    Animation, AnimationExt, AnyElement, App, ClickEvent, ElementId, FontFeatures, FontWeight,
    Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Svg, Transformation, Window, div, percentage,
    prelude::FluentBuilder, px, svg,
};
use gpui_component::tooltip::Tooltip;
use std::{rc::Rc, sync::Arc, time::Duration};

pub const MIC: &str = "icons/mic.svg";
pub const STOP: &str = "icons/square.svg";
pub const CLOSE: &str = "icons/x.svg";
pub const COPY: &str = "icons/copy.svg";
pub const CHECK: &str = "icons/check.svg";
pub const DELETE: &str = "icons/trash.svg";
pub const IMPORT: &str = "icons/file-music.svg";
pub const SEARCH: &str = "icons/search.svg";
pub const SPINNER: &str = "icons/loader-circle.svg";
pub const ALERT: &str = "icons/circle-alert.svg";
pub const DOWNLOAD: &str = "icons/download.svg";
pub const RETRY: &str = "icons/rotate-cw.svg";

pub fn icon(path: &'static str, size: Pixels, color: Hsla) -> Svg {
    svg()
        .path(SharedString::new_static(path))
        .size(size)
        .flex_none()
        .text_color(color)
}

/// A turning loader; a still one when motion is reduced.
pub fn spinner(id: impl Into<ElementId>, size: Pixels, color: Hsla, cx: &App) -> AnyElement {
    let still = icon(SPINNER, size, color);
    if cx.reduce_motion() {
        return still.into_any_element();
    }
    still
        .with_animation(
            id,
            Animation::new(Duration::from_millis(900)).repeat(),
            |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
        )
        .into_any_element()
}

pub fn tabular() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// The body face a step smaller: what labels are set in.
pub fn label(text: impl Into<SharedString>, color: Hsla) -> gpui::Div {
    div()
        .text_size(theme::LABEL)
        .text_color(color)
        .child(text.into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// The one filled action on a screen.
    Primary,
    /// A hairline outline.
    Outline,
    /// Bare until pointed at.
    Ghost,
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    tone: Tone,
    label: Option<SharedString>,
    icon: Option<&'static str>,
    spinning: bool,
    disabled: bool,
    small: bool,
    tooltip: Option<SharedString>,
    palette: Option<Tokens>,
    handler: Option<ClickHandler>,
}

pub fn button(id: impl Into<ElementId>, tone: Tone) -> Button {
    Button {
        id: id.into(),
        tone,
        label: None,
        icon: None,
        spinning: false,
        disabled: false,
        small: false,
        tooltip: None,
        palette: None,
        handler: None,
    }
}

impl Button {
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Replaces the icon with a turning loader.
    pub fn spinning(mut self, spinning: bool) -> Self {
        self.spinning = spinning;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    /// Names an icon-only button for pointers and screen readers.
    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn palette(mut self, palette: Tokens) -> Self {
        self.palette = Some(palette);
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.handler = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = self.palette.unwrap_or_else(|| theme::tokens(cx));
        let focus = window
            .use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone()
            .tab_stop(!self.disabled);
        let height = if self.small { px(28.) } else { px(32.) };
        let icon_size = if self.small { px(14.) } else { px(16.) };
        let clear = t.paper.opacity(0.);
        let (fg, bg, border, hover, press) = match self.tone {
            Tone::Primary => (
                t.paper,
                t.ink,
                t.ink,
                t.ink.opacity(0.85),
                t.ink.opacity(0.75),
            ),
            Tone::Outline => (t.ink, clear, t.rule, t.paper_2, t.paper_3),
            Tone::Ghost => (t.ink_2, clear, clear, t.paper_3, t.rule),
        };
        let handler = self.handler.clone();
        let disabled = self.disabled;
        let tooltip = self.tooltip.clone();
        let name = self.label.clone().or(self.tooltip.clone());
        let ring = t.ring();
        div()
            .id(self.id)
            .track_focus(&focus)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .h(height)
            .when(self.label.is_none(), |this| this.w(height))
            .when(self.label.is_some(), |this| {
                this.px(if self.small { px(12.) } else { px(16.) })
            })
            .rounded(px(999.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_color(fg)
            .text_size(if self.small { theme::LABEL } else { px(14.) })
            .font_weight(FontWeight::MEDIUM)
            .when_some(name, |this, name| this.aria_label(name))
            .when(disabled, |this| this.opacity(0.4))
            .when(!disabled, |this| {
                this.cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .active(move |style| style.bg(press))
            })
            .focus_visible(move |style| style.shadow(ring))
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                // Clicks in the top bar must not start a window move.
                window.prevent_default();
                cx.stop_propagation();
            })
            .when_some(handler.filter(|_| !disabled), |this, handler| {
                this.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx)
                })
            })
            .when_some(tooltip.filter(|_| self.label.is_none()), |this, tooltip| {
                this.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
            .map(|this| {
                if self.spinning {
                    this.child(spinner("spin", icon_size, fg, cx))
                } else if let Some(path) = self.icon {
                    this.child(icon(path, icon_size, fg))
                } else {
                    this
                }
            })
            .when_some(self.label, |this, label| this.child(label))
    }
}

pub fn keycap(text: impl Into<SharedString>, t: &Tokens) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .h(px(26.))
        .px(px(8.))
        .rounded(px(6.))
        .border_1()
        .border_color(t.rule)
        .bg(t.paper_2)
        .text_size(theme::LABEL)
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.ink)
        .child(text.into())
}

pub fn tag(text: impl Into<SharedString>, t: &Tokens) -> gpui::Div {
    div()
        .flex_none()
        .px(px(8.))
        .py(px(2.))
        .rounded(px(999.))
        .bg(t.paper_3)
        .text_size(theme::LABEL)
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.ink_2)
        .child(text.into())
}

/// History and Settings: the chosen one sits on a grey fill.
#[derive(IntoElement)]
pub struct Tabs {
    items: Vec<(&'static str, bool, Handler)>,
}

pub fn tabs() -> Tabs {
    Tabs { items: Vec::new() }
}

impl Tabs {
    pub fn tab(
        mut self,
        label: &'static str,
        selected: bool,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.items.push((label, selected, Rc::new(handler)));
        self
    }
}

impl RenderOnce for Tabs {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme::tokens(cx);
        div()
            .flex()
            .flex_none()
            .gap(px(2.))
            .children(self.items.into_iter().map(|(text, selected, handler)| {
                let focus = window
                    .use_keyed_state(text, cx, |_, cx| cx.focus_handle())
                    .read(cx)
                    .clone()
                    .tab_stop(true);
                let ring = t.ring();
                div()
                    .id(text)
                    .track_focus(&focus)
                    .flex()
                    .items_center()
                    .h(px(30.))
                    .px(px(14.))
                    .rounded(px(999.))
                    .text_size(px(14.))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .aria_label(text)
                    .aria_selected(selected)
                    .map(|this| {
                        if selected {
                            this.bg(t.paper_3).text_color(t.ink)
                        } else {
                            this.text_color(t.muted)
                                .hover(move |style| style.text_color(t.ink))
                        }
                    })
                    .focus_visible(move |style| style.shadow(ring))
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_click(move |_, window, cx| handler(window, cx))
                    .child(text)
            }))
    }
}
