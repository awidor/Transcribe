//! Settings: keys, the microphone, the shortcut, transcription and cleanup.
use crate::{
    controls::{self, Tone, button, icon, keycap, label, spinner, tabular},
    main_window::MainWindow,
    theme::{self, Tokens},
};
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::{input::Input, select::Select, switch::Switch, tooltip::Tooltip};
use transcribe_core::{
    provider::{CleanupEngine, TranscriptionModel},
    rules::{self, RecorderMode},
    s1,
};

const CONTROL_WIDTH: f32 = 340.;

struct Row {
    label: &'static str,
    error: Option<String>,
    control: AnyElement,
}

fn row(label: &'static str, control: impl IntoElement) -> Row {
    Row {
        label,
        error: None,
        control: control.into_any_element(),
    }
}

impl Row {
    fn error(mut self, error: Option<String>) -> Self {
        self.error = error.filter(|error| !error.is_empty());
        self
    }
}

/// A plate of rows, each a label and its control.
fn plate(title: &'static str, rows: Vec<Row>, t: Tokens) -> impl IntoElement + use<> {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .px(px(4.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .font_weight(FontWeight::MEDIUM)
                .text_size(px(16.))
                .text_color(t.ink)
                .child(title),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .rounded(theme::RADIUS_CARD)
                .bg(t.paper_2)
                .border_1()
                .border_color(t.rule)
                .children(rows.into_iter().enumerate().map(move |(index, row)| {
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(24.))
                        .min_h(px(58.))
                        .px(px(20.))
                        .py(px(10.))
                        .when(index > 0, |this| this.border_t_1().border_color(t.rule))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .min_w_0()
                                .child(
                                    div()
                                        .text_size(px(14.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(t.ink)
                                        .child(row.label),
                                )
                                .when_some(row.error, |this, error| {
                                    this.child(label(error, t.ink))
                                }),
                        )
                        .child(
                            div()
                                .w(px(CONTROL_WIDTH))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.))
                                .child(row.control),
                        )
                })),
        )
}

fn saved_mark(saved: bool, t: Tokens) -> Option<AnyElement> {
    saved.then(|| {
        div()
            .id(SharedString::new_static("saved"))
            .child(icon(controls::CHECK, px(16.), t.ink))
            .tooltip(|window, cx| Tooltip::new("Saved").build(window, cx))
            .into_any_element()
    })
}

impl MainWindow {
    pub(crate) fn settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme::tokens(cx);
        let openrouter = self.draft.cleanup_engine == CleanupEngine::OpenRouter;
        let local = self.draft.cleanup_engine == CleanupEngine::S1Mini;
        let voxtral =
            !self.draft.streaming && self.draft.transcription_model == TranscriptionModel::Voxtral;

        let mut transcription = vec![
            row(
                "Azure key",
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .w_full()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.azure_key).mask_toggle()),
                    )
                    .children(saved_mark(self.has_azure_key, t)),
            ),
            row("Microphone", Select::new(&self.microphone).w_full()),
            self.shortcut_row(cx),
            row(
                "Streaming",
                Switch::new("streaming")
                    .checked(self.draft.streaming)
                    .on_click(cx.listener(|this, on: &bool, window, cx| {
                        this.set_streaming(*on, window, cx)
                    })),
            ),
        ];
        if !self.draft.streaming {
            transcription.push(row(
                "Transcription",
                Select::new(&self.transcription).w_full(),
            ));
        }

        let mut cleanup = vec![row("Cleanup", Select::new(&self.engine).w_full())];
        if openrouter || voxtral {
            cleanup.push(row(
                "OpenRouter",
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .w_full()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.openrouter_key).mask_toggle()),
                    )
                    .children(saved_mark(self.has_openrouter_key, t)),
            ));
        }
        if openrouter {
            cleanup.push(self.model_row(cx));
            if self.models_open {
                cleanup.push(self.models_panel(cx));
            }
            cleanup.push(row(
                "Thinking level",
                Select::new(&self.effort)
                    .w_full()
                    .disabled(self.saving || self.efforts.is_empty()),
            ));
        }
        if local {
            cleanup.push(self.download_row("Engine", s1::Part::Engine, self.s1.engine.clone(), cx));
            cleanup.push(self.download_row("Model", s1::Part::Model, self.s1.model.clone(), cx));
            cleanup.push(row("Styling", Select::new(&self.styling).w_full()));
            cleanup.push(row("Unload model", Select::new(&self.unload).w_full()));
        }

        let updates = (self.update.phase != "unsupported").then(|| self.update_rows(cx));
        let _ = window;
        div().id("settings").size_full().overflow_y_scroll().child(
            div()
                .flex()
                .flex_col()
                .gap(px(28.))
                .w_full()
                .max_w(px(720.))
                .mx_auto()
                .px(px(24.))
                .py(px(28.))
                .child(plate("Transcription", transcription, t))
                .child(plate("Cleanup", cleanup, t))
                .child(
                    div().flex().justify_end().child(
                        button("save", Tone::Primary)
                            .label("Save")
                            .spinning(self.saving)
                            .disabled(self.saving || self.capturing())
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
                )
                .children(updates.map(|rows| plate("Updates", rows, t)))
                .child(div().h(px(48.))),
        )
    }

    fn shortcut_row(&mut self, cx: &mut Context<Self>) -> Row {
        let t = theme::tokens(cx);
        let mode = self.recorder.mode();
        let labels = if mode == RecorderMode::Listening {
            self.recorder.keys().to_vec()
        } else if let Some(label) = &self.draft.shortcut_label {
            vec![label.clone()]
        } else {
            rules::shortcut_labels(&self.draft.shortcut)
        };
        let keys: AnyElement = match mode {
            RecorderMode::Starting => spinner("shortcut-starting", px(16.), t.muted, cx),
            _ if labels.is_empty() => label("Press keys", t.muted).into_any_element(),
            _ => div()
                .flex()
                .gap(px(4.))
                .children(labels.into_iter().map(|key| keycap(key, &t)))
                .into_any_element(),
        };
        let capturing = mode != RecorderMode::Idle;
        let saving = self.saving;
        row(
            "Shortcut",
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .id("shortcut-keys")
                        .flex()
                        .items_center()
                        .when(!capturing && !saving, |this| {
                            this.cursor_pointer()
                                .on_mouse_down(MouseButton::Left, |_, window, _| {
                                    window.prevent_default()
                                })
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.begin_capture(window, cx)
                                    }),
                                )
                        })
                        .child(keys),
                )
                .map(|this| {
                    if capturing {
                        this.child(
                            button("shortcut-cancel", Tone::Ghost)
                                .icon(controls::CLOSE)
                                .small()
                                .tooltip("Cancel shortcut")
                                .on_click(cx.listener(|this, _, _, cx| this.stop_capture(cx))),
                        )
                    } else {
                        this.child(
                            button("shortcut-change", Tone::Outline)
                                .label("Change")
                                .small()
                                .disabled(saving)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.begin_capture(window, cx)
                                })),
                        )
                    }
                }),
        )
        .error(self.shortcut_error.clone())
    }

    fn model_row(&mut self, cx: &mut Context<Self>) -> Row {
        let t = theme::tokens(cx);
        let side: Option<AnyElement> = if self.loading_models {
            Some(spinner("models-loading", px(16.), t.muted, cx))
        } else if self.models_failed {
            Some(
                button("models-retry", Tone::Ghost)
                    .icon(controls::RETRY)
                    .small()
                    .tooltip("Retry")
                    .on_click(cx.listener(|this, _, window, cx| this.load_models(window, cx)))
                    .into_any_element(),
            )
        } else if !self.models.is_empty() {
            Some(
                button("models", Tone::Outline)
                    .label("Models")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.models_open = !this.models_open;
                        cx.notify();
                    }))
                    .into_any_element(),
            )
        } else {
            None
        };
        row(
            "Cleanup model",
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .w_full()
                .child(div().flex_1().child(Input::new(&self.model)))
                .children(side),
        )
        .error(self.models_failed.then(|| "Models unavailable".to_string()))
    }

    fn models_panel(&mut self, cx: &mut Context<Self>) -> Row {
        let t = theme::tokens(cx);
        let search = self.models_search.read(cx).value().to_string();
        let shown: Vec<(String, String)> = self
            .models
            .iter()
            .filter(|model| rules::matches(&format!("{} {}", model.id, model.name), &search))
            .map(|model| (model.id.clone(), model.name.clone()))
            .collect();
        let empty = shown.is_empty();
        row(
            "Models",
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .w_full()
                .child(Input::new(&self.models_search).prefix(icon(
                    controls::SEARCH,
                    px(15.),
                    t.muted,
                )))
                .child(
                    div()
                        .id("models-list")
                        .flex()
                        .flex_col()
                        .max_h(px(240.))
                        .overflow_y_scroll()
                        .rounded(theme::RADIUS_PART)
                        .border_1()
                        .border_color(t.rule)
                        .bg(t.paper)
                        .when(empty, |this| {
                            this.child(div().p(px(12.)).child(label("No matches", t.muted)))
                        })
                        .children(shown.into_iter().map(|(id, name)| {
                            let chosen = id.clone();
                            div()
                                .id(SharedString::from(format!("model-{id}")))
                                .flex()
                                .flex_col()
                                .px(px(10.))
                                .py(px(6.))
                                .cursor_pointer()
                                .hover(move |s| s.bg(t.paper_2))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.choose_model(chosen.clone(), window, cx)
                                }))
                                .child(div().text_size(px(14.)).text_color(t.ink).child(name))
                                .child(label(id, t.muted))
                        })),
                ),
        )
    }

    fn download_row(
        &mut self,
        title: &'static str,
        part: s1::Part,
        download: s1::Download,
        cx: &mut Context<Self>,
    ) -> Row {
        let t = theme::tokens(cx);
        let id = match part {
            s1::Part::Engine => "engine",
            s1::Part::Model => "model",
        };
        let control: AnyElement = if let Some(progress) = download.progress {
            let fraction = progress.clamp(0., 1.) as f32;
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .w(px(120.))
                        .h(px(6.))
                        .rounded(px(999.))
                        .bg(t.paper_3)
                        .child(
                            div()
                                .h_full()
                                .w(px(120. * fraction))
                                .rounded(px(999.))
                                .bg(t.ink),
                        ),
                )
                .child(
                    label(format!("{}%", (progress * 100.).floor() as i64), t.ink)
                        .font_features(tabular()),
                )
                .child(
                    button(SharedString::from(format!("cancel-{id}")), Tone::Ghost)
                        .icon(controls::CLOSE)
                        .small()
                        .tooltip(match part {
                            s1::Part::Engine => "Cancel engine download",
                            s1::Part::Model => "Cancel model download",
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.download(part, true, window, cx)
                        })),
                )
                .into_any_element()
        } else if download.ready {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(icon(controls::CHECK, px(16.), t.ink))
                .child(label("Downloaded", t.ink))
                .into_any_element()
        } else {
            button(SharedString::from(format!("download-{id}")), Tone::Outline)
                .icon(controls::DOWNLOAD)
                .label(format!("Download {}", rules::size_label(download.size)))
                .small()
                .on_click(
                    cx.listener(move |this, _, window, cx| this.download(part, false, window, cx)),
                )
                .into_any_element()
        };
        row(title, control).error(download.error)
    }

    fn update_rows(&mut self, cx: &mut Context<Self>) -> Vec<Row> {
        let update = &self.update;
        let busy = rules::update_busy(update);
        let recording = rules::busy(self.session.phase);
        let status = rules::update_status(update).unwrap_or_default();
        let t = theme::tokens(cx);
        let version = format!("Version {}", update.current);
        vec![Row {
            label: "Version",
            error: (update.phase == "error")
                .then(|| update.error.clone())
                .flatten(),
            control: div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(label(version, t.muted).font_features(tabular()))
                .when(!status.is_empty(), |this| this.child(label(status, t.ink)))
                .child(
                    button("update-action", Tone::Outline)
                        .label(rules::update_action(update))
                        .small()
                        .spinning(busy)
                        .disabled(busy || (update.phase == "available" && recording))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.update_action(window, cx)),
                        ),
                )
                .into_any_element(),
        }]
    }
}
