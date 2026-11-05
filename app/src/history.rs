//! History: every transcript, searchable, and the one chosen open for editing.
use crate::{
    controls::{self, Tone, button, icon, label, spinner, tabular, tag},
    main_window::{MainWindow, Tab, date},
    theme,
};
use gpui::{
    Context, InteractiveElement, IntoElement, MouseButton, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px, relative,
    uniform_list,
};
use gpui_component::input::{Input, Textarea};
use std::ops::Range;
use transcribe_core::rules;

const LIST_WIDTH: f32 = 340.;
const ROW_HEIGHT: f32 = 90.;

impl MainWindow {
    pub(crate) fn history(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme::tokens(cx);
        div()
            .size_full()
            .flex()
            .child(
                div()
                    .w(px(LIST_WIDTH))
                    .flex_none()
                    .h_full()
                    .flex()
                    .flex_col()
                    .border_r_1()
                    .border_color(t.rule)
                    .child(self.list_head(cx))
                    .child(self.list(window, cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.detail(window, cx)),
            )
    }

    fn list_head(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme::tokens(cx);
        let searching = !self.search.read(cx).value().is_empty();
        let count = self.shown(cx).len();
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(16.))
            .pb(px(10.))
            .child(
                Input::new(&self.search)
                    .prefix(icon(controls::SEARCH, px(15.), t.muted))
                    .cleanable(true),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(label(
                        if searching {
                            "Search results"
                        } else {
                            "All transcripts"
                        },
                        t.muted,
                    ))
                    .child(label(count.to_string(), t.muted).font_features(tabular())),
            )
    }

    fn list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::tokens(cx);
        let center = |child: gpui::AnyElement| {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .p(px(24.))
                .child(child)
        };
        if !self.loaded {
            return center(spinner("history-loading", px(20.), t.muted, cx)).into_any_element();
        }
        if self.history_failed {
            return center(label("History unavailable", t.muted).into_any_element())
                .child(
                    button("history-retry", Tone::Outline)
                        .icon(controls::RETRY)
                        .label("Retry")
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                )
                .into_any_element();
        }
        let shown = self.shown(cx);
        if shown.is_empty() {
            let searching = !self.search.read(cx).value().is_empty();
            return center(
                label(
                    if searching {
                        "No matches"
                    } else {
                        "No transcripts"
                    },
                    t.muted,
                )
                .into_any_element(),
            )
            .into_any_element();
        }
        let _ = window;
        uniform_list(
            "entries",
            shown.len(),
            cx.processor(move |this, range: Range<usize>, window, cx| {
                range
                    .filter_map(|index| shown.get(index).copied())
                    .map(|index| this.row(index, window, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.list_scroll)
        .flex_1()
        .into_any_element()
    }

    fn row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme::tokens(cx);
        let entry = &self.entries[index];
        let id = entry.id.clone();
        let selected = self.selected.as_deref() == Some(id.as_str());
        let failed = entry.status == "failed";
        let focus = window
            .use_keyed_state(
                gpui::SharedString::from(format!("row-{id}")),
                cx,
                |_, cx| cx.focus_handle(),
            )
            .read(cx)
            .clone()
            .tab_stop(true);
        let preview = rules::entry_preview(entry, &self.session);
        let seconds = format!("{}s", entry.seconds.round() as i64);
        let when = date(entry.created_at);
        let card = div()
            .id(gpui::SharedString::from(id.clone()))
            .track_focus(&focus)
            .size_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(10.))
            .border_1()
            .cursor_pointer()
            .aria_selected(selected)
            .map(|this| {
                if selected {
                    this.bg(t.paper_3).border_color(t.paper_3)
                } else {
                    this.border_color(t.paper.opacity(0.))
                        .hover(move |s| s.bg(t.paper_2))
                }
            })
            .focus_visible(move |s| s.border_color(t.focus))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(
                cx.listener(move |this, _, window, cx| this.pick_entry(id.clone(), window, cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(label(when, t.muted))
                    .map(|this| {
                        if failed {
                            this.child(icon(controls::ALERT, px(14.), t.ink))
                        } else {
                            this.child(label(seconds, t.muted).font_features(tabular()))
                        }
                    }),
            )
            .child(
                div()
                    .line_clamp(2)
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .text_color(t.ink)
                    .child(preview),
            );
        div().h(px(ROW_HEIGHT)).px(px(8.)).py(px(3.)).child(card)
    }

    fn detail(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::tokens(cx);
        let _ = window;
        let Some(entry) = self
            .selected
            .as_ref()
            .and_then(|id| self.entries.iter().find(|entry| &entry.id == id))
            .cloned()
        else {
            if !self.loaded {
                return div().into_any_element();
            }
            let has_key = self.has_azure_key;
            let busy = rules::working(self.session.phase)
                || self.session.phase == transcribe_core::session::Phase::Recording;
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(16.))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_size(theme::TITLE)
                        .text_color(t.ink)
                        .child("No transcripts yet"),
                )
                .child(
                    button("empty-action", Tone::Outline)
                        .label(if has_key {
                            "Import audio"
                        } else {
                            "Open Settings"
                        })
                        .disabled(has_key && busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if has_key {
                                this.import_audio(window, cx);
                            } else {
                                this.show_tab(Tab::Settings, cx);
                            }
                        })),
                )
                .into_any_element();
        };
        let value = self.text_value(cx);
        let words = rules::word_count(&value);
        let not_pasted = rules::not_pasted(&entry);
        let error = entry
            .error
            .clone()
            .filter(|error| !error.is_empty() && !not_pasted);
        div()
            .size_full()
            .flex()
            .flex_col()
            .p(px(24.))
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_size(theme::TITLE)
                            .text_color(t.ink)
                            .child(date(entry.created_at)),
                    )
                    .when(not_pasted, |this| {
                        let reason = entry.error.clone().unwrap_or_default();
                        this.child(div().id("not-pasted").child(tag("Not pasted", &t)).when(
                            !reason.is_empty(),
                            |this| {
                                this.tooltip(move |window, cx| {
                                    gpui_component::tooltip::Tooltip::new(reason.clone())
                                        .build(window, cx)
                                })
                            },
                        ))
                    })
                    .child(div().flex_1())
                    .child(
                        button("copy", Tone::Outline)
                            .icon(if self.copied.is_some() {
                                controls::CHECK
                            } else {
                                controls::COPY
                            })
                            .tooltip(if self.copied.is_some() {
                                "Copied"
                            } else {
                                "Copy"
                            })
                            .disabled(value.is_empty())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.copy_entry(window, cx)),
                            ),
                    )
                    .child(
                        button("delete", Tone::Ghost)
                            .icon(controls::DELETE)
                            .tooltip("Delete")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.delete_entry(window, cx)),
                            ),
                    ),
            )
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(theme::RADIUS_PART)
                        .bg(t.paper_2)
                        .text_size(px(14.))
                        .text_color(t.ink)
                        .child(icon(controls::ALERT, px(15.), t.ink))
                        .child(div().flex_1().min_w_0().child(error)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .text_size(px(16.))
                    .line_height(px(26.))
                    .child(
                        Textarea::new(&self.text)
                            .h(relative(1.))
                            .text_size(px(16.))
                            .line_height(px(26.)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(16.))
                    .child(label(format!("{words} words"), t.muted).font_features(tabular()))
                    .child(
                        label(format!("{} seconds", entry.seconds.round() as i64), t.muted)
                            .font_features(tabular()),
                    ),
            )
            .into_any_element()
    }
}
