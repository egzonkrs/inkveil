//! "Improve" and "Humanize" on an open note: sends the selected text, shows progress while the
//! model works, shows the suggestion, and replaces the selection when the user accepts it.

use std::{
    ops::Range,
    time::{Duration, Instant},
};

use gpui::{AnyElement, Context, IntoElement, Task, Window, div, prelude::*, px, relative};
use inkveil_doc::Selection;

use super::note_editor::{NoteEditor, NoteEditorEvent};
use crate::{
    ai::{self, AiConfig, AiMode},
    assets::Icon,
    theme::theme,
    ui::{button, icon, icon_button},
};

enum State {
    /// The text looks like it contains secrets; nothing is sent unless the user says so.
    ConfirmSecrets(Vec<crate::secrets::Finding>),
    Working(Working),
    Ready {
        text: String,
        changed: Vec<Range<usize>>,
    },
    /// Nothing was sent (no selection, too long, …).
    Failed(String),
    /// The request failed; it can be tried again.
    RequestFailed(String),
}

/// A request on its way.
struct Working {
    started: Instant,
    /// The retry under way (1 to [`ai::MAX_RETRIES`]), or 0 for the first attempt.
    retry: u32,
    phase: Phase,
}

enum Phase {
    Waiting,
    Thinking,
    /// Characters of the reply received so far.
    Writing(usize),
    /// Pausing before the next retry.
    Pause {
        reason: String,
        until: Instant,
    },
}

impl Working {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            retry: 0,
            phase: Phase::Waiting,
        }
    }

    fn apply(&mut self, progress: ai::Progress) {
        match progress {
            ai::Progress::Waiting { attempt } => {
                self.retry = attempt.saturating_sub(1);
                self.phase = Phase::Waiting;
            }
            ai::Progress::Thinking => self.phase = Phase::Thinking,
            ai::Progress::Writing { chars } => self.phase = Phase::Writing(chars),
            ai::Progress::Retrying {
                retry,
                reason,
                wait,
            } => {
                self.retry = retry;
                self.phase = Phase::Pause {
                    reason,
                    until: Instant::now() + wait,
                };
            }
        }
    }
}

/// The suggestion card of one open note.
pub struct AiPanel {
    mode: AiMode,
    original: String,
    selection: Selection,
    /// Document version when the text was sent; the suggestion only applies to that version.
    version: u64,
    state: State,
    _request: Option<Task<()>>,
}

impl NoteEditor {
    pub fn ai_open(&self) -> bool {
        self.ai.is_some()
    }

    pub fn ai_mode(&self) -> Option<AiMode> {
        self.ai.as_ref().map(|p| p.mode)
    }

    /// Sends the selected text for light copy-editing. Nothing is sent without a selection.
    pub fn improve_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_ai_rewrite(AiMode::Improve, window, cx);
    }

    /// Sends the selected text to be rewritten in plain, natural words.
    pub fn humanize_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_ai_rewrite(AiMode::Humanize, window, cx);
    }

    fn start_ai_rewrite(&mut self, mode: AiMode, window: &mut Window, cx: &mut Context<Self>) {
        let (selection, version, text, crosses_table) = {
            let ed = self.editor.read(cx).editor();
            (
                ed.selection(),
                ed.version(),
                ed.selected_plain_text(),
                ed.selection_crosses_table(),
            )
        };
        let fail = |this: &mut Self, message: &str, cx: &mut Context<Self>| {
            this.ai = Some(AiPanel {
                mode,
                original: String::new(),
                selection,
                version,
                state: State::Failed(message.to_owned()),
                _request: None,
            });
            cx.notify();
        };
        if selection.is_collapsed() || text.trim().is_empty() {
            return fail(
                self,
                match mode {
                    AiMode::Improve => {
                        "Select the text you want to improve first. Only the selected text is sent."
                    }
                    AiMode::Humanize => {
                        "Select the text you want to humanize first. Only the selected text is sent."
                    }
                },
                cx,
            );
        }
        if self.editor.read(cx).editor().has_multiple_carets() {
            return fail(self, "Use a single selection for this.", cx);
        }
        if crosses_table {
            return fail(
                self,
                "Select text outside tables, or inside a single table cell.",
                cx,
            );
        }
        if text.chars().count() > ai::MAX_CHARS {
            return fail(
                self,
                "That selection is too long. Select up to about 8,000 characters at a time.",
                cx,
            );
        }
        let Some(config) = AiConfig::get(cx) else {
            cx.emit(NoteEditorEvent::AiSettingsRequested);
            return;
        };
        // Check for keys, passwords and other secrets before anything leaves the computer.
        let findings = crate::secrets::scan(&text);
        if !findings.is_empty() {
            self.ai = Some(AiPanel {
                mode,
                original: text,
                selection,
                version,
                state: State::ConfirmSecrets(findings),
                _request: None,
            });
            cx.notify();
            return;
        }
        self.send_for_rewrite(mode, config, text, (selection, version), window, cx);
    }

    /// Sends the text again: after the user confirmed it may contain secrets, or after a failed
    /// request. The selection and note must not have changed in between.
    fn resend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.ai.take() else {
            return;
        };
        let current = {
            let ed = self.editor.read(cx).editor();
            (ed.selection(), ed.version())
        };
        if current != (panel.selection, panel.version) {
            self.ai = Some(AiPanel {
                state: State::Failed(
                    "The selection changed. Select the text again and retry.".into(),
                ),
                ..panel
            });
            cx.notify();
            return;
        }
        let Some(config) = AiConfig::get(cx) else {
            cx.emit(NoteEditorEvent::AiSettingsRequested);
            return;
        };
        self.send_for_rewrite(
            panel.mode,
            config,
            panel.original,
            (panel.selection, panel.version),
            window,
            cx,
        );
    }

    fn send_for_rewrite(
        &mut self,
        mode: AiMode,
        config: AiConfig,
        text: String,
        (selection, version): (Selection, u64),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The request runs on its own thread (it blocks on the network); progress comes back over
        // a channel. Closing the panel drops the receiving end, which stops the request.
        let (progress_tx, mut progress_rx) = futures::channel::mpsc::unbounded();
        let (done_tx, mut done_rx) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("inkveil-ai".into())
            .spawn({
                let text = text.clone();
                move || {
                    let _ = done_tx.send(ai::rewrite(&config, &text, mode, &progress_tx));
                }
            });
        let state = if spawned.is_ok() {
            State::Working(Working::new())
        } else {
            State::RequestFailed("Could not start the request.".into())
        };
        let task = cx.spawn_in(window, async move |this, cx| {
            loop {
                // Also drives the progress bar's animation and the seconds counter.
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let mut updates = Vec::new();
                while let Ok(progress) = progress_rx.try_recv() {
                    updates.push(progress);
                }
                let result = done_rx.try_recv();
                let finished = !matches!(result, Ok(None));
                let open = this
                    .update(cx, |this, cx| {
                        let Some(panel) = this.ai.as_mut() else {
                            return false;
                        };
                        if let State::Working(working) = &mut panel.state {
                            for progress in updates {
                                working.apply(progress);
                            }
                        }
                        match result {
                            Ok(None) => {}
                            Ok(Some(Ok(text))) if text == panel.original => {
                                panel.state = State::Failed(
                                    match panel.mode {
                                        AiMode::Improve => {
                                            "No changes needed: the text already reads well."
                                        }
                                        AiMode::Humanize => {
                                            "No changes needed: the text already reads naturally."
                                        }
                                    }
                                    .into(),
                                );
                            }
                            Ok(Some(Ok(text))) => {
                                panel.state = State::Ready {
                                    changed: ai::changed_ranges(&panel.original, &text),
                                    text,
                                };
                            }
                            Ok(Some(Err(error))) => panel.state = State::RequestFailed(error.0),
                            Err(_) => {
                                panel.state = State::RequestFailed(
                                    "The request stopped unexpectedly.".into(),
                                );
                            }
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if finished || !open {
                    break;
                }
            }
        });
        self.ai = Some(AiPanel {
            mode,
            original: text,
            selection,
            version,
            state,
            _request: Some(task),
        });
        cx.notify();
    }

    /// Replaces the selection with the suggestion, as one undoable step.
    fn accept_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.ai.as_ref() else {
            return;
        };
        let State::Ready { text, .. } = &panel.state else {
            return;
        };
        if self.editor.read(cx).editor().version() != panel.version {
            let mode = panel.mode;
            let selection = panel.selection;
            let version = panel.version;
            self.ai = Some(AiPanel {
                mode,
                original: String::new(),
                selection,
                version,
                state: State::Failed(
                    "The note changed while waiting for the suggestion. Select the text again and \
                     retry."
                        .into(),
                ),
                _request: None,
            });
            cx.notify();
            return;
        }
        let (selection, text) = (panel.selection, text.clone());
        self.editor.update(cx, |e, cx| {
            e.run(cx, |ed| {
                ed.set_selection(selection);
                if !ed.replace_selected_lines(&text) {
                    ed.insert_text(&text);
                }
            });
            e.focus(window, cx);
        });
        self.ai = None;
        cx.notify();
    }

    pub fn close_ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ai = None;
        self.editor.update(cx, |e, cx| e.focus(window, cx));
        cx.notify();
    }

    /// The suggestion card, floating at the top right of the editor.
    pub fn render_ai(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = self.ai.as_ref()?;
        let colors = theme(cx);
        let (header_icon, header_title, working_label, footer_hint) = match panel.mode {
            AiMode::Improve => (
                Icon::Sparkles,
                "Improve with AI",
                "Improving the selected text…",
                "Changed words are highlighted. Replace can be undone with Ctrl+Z.",
            ),
            AiMode::Humanize => (
                Icon::NotebookPen,
                "Humanize with AI",
                "Rewriting the selected text in plain, natural words…",
                "Changed words are highlighted. Hidden characters are removed. Replace can be undone \
                 with Ctrl+Z.",
            ),
        };
        let body: AnyElement = match &panel.state {
            State::ConfirmSecrets(findings) => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(colors.error)
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(colors.error)
                                .child("This text may contain secrets"),
                        )
                        .children(findings.iter().map(|f| {
                            div()
                                .text_size(px(12.))
                                .child(format!("• {} ({})", f.kind, f.hint))
                        })),
                )
                .child(div().text_color(colors.muted).child(
                    "Sending it would share these with OpenRouter and the model provider. Remove them from the selection first, or send anyway if they are not real.",
                ))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .justify_end()
                        .child(button("secrets-cancel", "Cancel", true, cx).on_click(
                            cx.listener(|this, _, window, cx| this.close_ai(window, cx)),
                        ))
                        .child(button("secrets-send", "Send anyway", false, cx).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.resend(window, cx);
                            }),
                        )),
                )
                .into_any_element(),
            State::Working(working) => {
                self.render_progress(working, working_label, &panel.original, cx)
            }
            State::Failed(message) => div().child(message.clone()).into_any_element(),
            State::RequestFailed(message) => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_color(colors.error).child(message.clone()))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .justify_end()
                        .child(button("ai-close-failed", "Close", false, cx).on_click(
                            cx.listener(|this, _, window, cx| this.close_ai(window, cx)),
                        ))
                        .child(button("ai-try-again", "Try again", true, cx).on_click(
                            cx.listener(|this, _, window, cx| this.resend(window, cx)),
                        )),
                )
                .into_any_element(),
            State::Ready { text, changed } => div()
                .id("ai-suggestion")
                .max_h(px(360.))
                .overflow_y_scroll()
                .p_3()
                .rounded_md()
                .bg(colors.background)
                .border_1()
                .border_color(colors.border)
                .text_size(px(14.))
                .line_height(px(22.))
                .child(
                    gpui::StyledText::new(text.clone()).with_highlights(changed.iter().map(|r| {
                        (
                            r.clone(),
                            gpui::HighlightStyle {
                                background_color: Some(colors.selection),
                                ..Default::default()
                            },
                        )
                    })),
                )
                .into_any_element(),
        };
        let ready = matches!(panel.state, State::Ready { .. });
        let top = if self.find_open() { px(96.) } else { px(6.) };
        Some(
            div()
                .id("ai-panel")
                .occlude()
                .absolute()
                .top(top)
                .right(px(28.))
                .w(px(560.))
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(colors.border)
                .bg(colors.surface)
                .shadow_lg()
                .text_size(px(13.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(icon(header_icon, colors.accent))
                        .child(
                            div()
                                .flex_1()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(header_title),
                        )
                        .child(
                            icon_button(
                                "ai-settings",
                                Icon::Settings,
                                false,
                                "API key and model",
                                cx,
                            )
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(NoteEditorEvent::AiSettingsRequested);
                            })),
                        )
                        .child(
                            icon_button("close-ai", Icon::X, false, "Close (Escape)", cx).on_click(
                                cx.listener(|this, _, window, cx| this.close_ai(window, cx)),
                            ),
                        ),
                )
                .child(body)
                .when(ready, |d| {
                    d.child(
                        div()
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child(footer_hint),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .justify_end()
                            .child(button("ai-discard", "Discard", false, cx).on_click(
                                cx.listener(|this, _, window, cx| this.close_ai(window, cx)),
                            ))
                            .child(button("ai-copy", "Copy", false, cx).on_click(cx.listener(
                                |this, _, _, cx| {
                                    if let Some(AiPanel {
                                        state: State::Ready { text, .. },
                                        ..
                                    }) = &this.ai
                                    {
                                        crate::clipboard::write_text(text, cx);
                                    }
                                },
                            )))
                            .child(
                                button("ai-replace", "Replace selection", true, cx).on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.accept_suggestion(window, cx);
                                    }),
                                ),
                            ),
                    )
                })
                .into_any_element(),
        )
    }
}

/// The model and effort in words, such as "Grok 4.7, medium effort".
fn model_label(config: &AiConfig) -> String {
    let id = config.model_or_default();
    let name = ai::model_choice(id).map_or(id, |m| m.name);
    match config.effort_or_default() {
        ai::Effort::Default => name.to_owned(),
        ai::Effort::None => format!("{name}, no reasoning"),
        effort => format!("{name}, {} effort", effort.label().to_lowercase()),
    }
}

impl NoteEditor {
    /// The progress bar and what the request is doing.
    fn render_progress(
        &self,
        working: &Working,
        label: &'static str,
        original: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme(cx);
        let config = AiConfig::get(cx);
        let name = config
            .as_ref()
            .map_or("the model", |c| model_name(c.model_or_default()));
        let elapsed = working.started.elapsed();
        let retry = if working.retry > 0 {
            format!("Retry {} of {}. ", working.retry, ai::MAX_RETRIES)
        } else {
            String::new()
        };
        // How far along: known while writing (replies are about as long as the selection),
        // otherwise a bar that slides back and forth.
        let (fraction, status, paused) = match &working.phase {
            Phase::Waiting => (None, format!("{retry}Waiting for {name}…"), false),
            Phase::Thinking => (None, format!("{retry}{name} is thinking…"), false),
            Phase::Writing(chars) => {
                let expected = original.chars().count().max(1);
                #[allow(clippy::cast_precision_loss)]
                let fraction = (*chars as f32 / expected as f32).min(0.97);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let percent = (fraction * 100.).round() as u32;
                (Some(fraction), format!("{retry}Writing… {percent}%"), false)
            }
            Phase::Pause { reason, until } => {
                let left = until.saturating_duration_since(Instant::now()).as_secs() + 1;
                (
                    None,
                    format!(
                        "Retrying {} of {} in {left} s. {reason}.",
                        working.retry,
                        ai::MAX_RETRIES
                    ),
                    true,
                )
            }
        };
        let bar = if let Some(fraction) = fraction {
            div().left_0().w(relative(fraction))
        } else {
            const PERIOD_MS: u128 = 1600;
            #[allow(clippy::cast_precision_loss)]
            let t = (elapsed.as_millis() % PERIOD_MS) as f32 / PERIOD_MS as f32;
            div().left(relative(t * 1.35 - 0.35)).w(relative(0.35))
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().child(label))
            .child(
                div()
                    .relative()
                    .h(px(4.))
                    .w_full()
                    .rounded_full()
                    .overflow_hidden()
                    .bg(colors.border)
                    .child(
                        bar.absolute()
                            .top_0()
                            .bottom_0()
                            .rounded_full()
                            .bg(if paused { colors.muted } else { colors.accent }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .text_size(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .text_color(if paused { colors.error } else { colors.muted })
                            .child(status),
                    )
                    .child(
                        div()
                            .text_color(colors.muted)
                            .child(format!("{} s", elapsed.as_secs())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .children(
                                config
                                    .as_ref()
                                    .map(|c| format!("Using {}.", model_label(c))),
                            ),
                    )
                    .child(
                        button("ai-stop", "Stop", false, cx)
                            .on_click(cx.listener(|this, _, window, cx| this.close_ai(window, cx))),
                    ),
            )
            .into_any_element()
    }
}

/// The model's name, such as "Grok 4.7", or its id for a model typed in by hand.
fn model_name(id: &str) -> &str {
    ai::model_choice(id).map_or(id, |m| m.name)
}
