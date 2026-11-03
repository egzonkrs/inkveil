//! The "AI writing help" settings dialog: the OpenRouter API key, model and reasoning effort.

use gpui::{
    AnyElement, Context, Entity, Focusable, IntoElement, Subscription, Window, deferred, div,
    prelude::*, px,
};
use zeroize::Zeroizing;

use super::Workspace;
use crate::{
    ai::{AiConfig, DEFAULT_MODEL, Effort, MODELS, OTHER_MODEL_EFFORTS, model_choice},
    assets::Icon,
    storage::state_keys,
    theme::theme,
    ui::{
        button, icon_button,
        text_field::{TextField, TextFieldEvent},
    },
};

/// The open dialog.
pub struct AiDialog {
    key: Entity<TextField>,
    model: Entity<TextField>,
    effort: Effort,
    show_key: bool,
    /// Shown above the fields, e.g. why the dialog opened.
    note: Option<&'static str>,
    _model_changed: Subscription,
}

impl AiDialog {
    /// The efforts the model in the model field offers.
    fn efforts(&self, cx: &gpui::App) -> &'static [Effort] {
        model_choice(self.model.read(cx).text()).map_or(OTHER_MODEL_EFFORTS, |m| m.efforts)
    }

    /// Keeps the chosen effort one the model offers, else takes the model's preset.
    fn fit_effort(&mut self, cx: &gpui::App) {
        if !self.efforts(cx).contains(&self.effort) {
            self.effort =
                model_choice(self.model.read(cx).text()).map_or(Effort::Default, |m| m.effort);
        }
    }
}

impl Workspace {
    /// Loads the saved configuration (after unlocking).
    pub(super) fn load_ai_config(&self, bytes: Option<&[u8]>, cx: &mut Context<Self>) {
        cx.set_global(bytes.map(AiConfig::decode).unwrap_or_default());
    }

    pub(super) fn open_ai_settings(
        &mut self,
        note: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_settings = false;
        let current = cx.try_global::<AiConfig>().cloned().unwrap_or_default();
        let key = cx.new(|cx| {
            let mut field = TextField::new("sk-or-v1-…", window, cx).masked();
            field.set_text(&current.api_key, cx);
            field
        });
        let model = cx.new(|cx| {
            let mut field = TextField::new(DEFAULT_MODEL, window, cx);
            field.set_text(current.model_or_default(), cx);
            field
        });
        // Typing another model id updates the cards and the efforts on offer.
        let model_changed = cx.subscribe(&model, |this, _, event, cx| {
            if *event == TextFieldEvent::Changed
                && let Some(dialog) = this.ai_dialog.as_mut()
            {
                dialog.fit_effort(cx);
                cx.notify();
            }
        });
        window.focus(&key.focus_handle(cx), cx);
        self.ai_dialog = Some(AiDialog {
            key,
            model,
            effort: current.effort_or_default(),
            show_key: false,
            note,
            _model_changed: model_changed,
        });
        cx.notify();
    }

    fn pick_model(&mut self, id: &'static str, cx: &mut Context<Self>) {
        let Some(dialog) = self.ai_dialog.as_mut() else {
            return;
        };
        dialog.model.update(cx, |field, cx| field.set_text(id, cx));
        if let Some(choice) = model_choice(id) {
            dialog.effort = choice.effort;
        }
        cx.notify();
    }

    fn save_ai_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.ai_dialog.take() else {
            return;
        };
        let model = dialog.model.read(cx).text().trim();
        let config = AiConfig {
            api_key: Zeroizing::new(dialog.key.read(cx).text().trim().to_owned()),
            model: if model.is_empty() {
                DEFAULT_MODEL.to_owned()
            } else {
                model.to_owned()
            },
            effort: dialog.effort,
        };
        self.storage.put_state(state_keys::AI, config.encode());
        cx.set_global(config);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn remove_ai_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ai_dialog = None;
        let current = cx.try_global::<AiConfig>().cloned().unwrap_or_default();
        let config = AiConfig {
            api_key: Zeroizing::default(),
            ..current
        };
        self.storage.put_state(state_keys::AI, config.encode());
        cx.set_global(config);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    pub(super) fn render_ai_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.ai_dialog.as_ref()?;
        let colors = theme(cx);
        let saved = AiConfig::get(cx);
        let label = |text: &'static str| {
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(colors.muted)
                .child(text)
        };
        let model_id = dialog.model.read(cx).text().trim().to_owned();
        let picked = model_choice(&model_id).map(|m| m.id);
        let model_cards = div().flex().gap_2().children(MODELS.iter().map(|m| {
            let selected = picked == Some(m.id);
            let id = m.id;
            div()
                .id(m.id)
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(if selected {
                    colors.accent
                } else {
                    colors.border
                })
                .when(selected, |d| d.bg(colors.selected))
                .cursor_pointer()
                .hover(|d| d.bg(colors.hover))
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(m.name))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.muted)
                        .child(m.about),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.pick_model(id, cx)))
        }));
        let effort_chips =
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(dialog.efforts(cx).iter().map(|&effort| {
                    let selected = dialog.effort == effort;
                    div()
                        .id(effort.label())
                        .h(px(28.))
                        .px_3()
                        .flex()
                        .items_center()
                        .rounded_md()
                        .border_1()
                        .border_color(if selected {
                            colors.accent
                        } else {
                            colors.border
                        })
                        .when(selected, |d| d.bg(colors.selected))
                        .cursor_pointer()
                        .text_size(px(12.))
                        .hover(|d| d.bg(colors.hover))
                        .child(effort.label())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(dialog) = this.ai_dialog.as_mut() {
                                dialog.effort = effort;
                                cx.notify();
                            }
                        }))
                }));
        Some(
            deferred(
                div()
                    .id("ai-dialog-backdrop")
                    .occlude()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgba(0x0000_0088))
                    .child(
                        div()
                            .w(px(520.))
                            .p_6()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .rounded_lg()
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.background)
                            .shadow_lg()
                            .text_size(px(13.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(px(16.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(crate::ui::icon(Icon::Sparkles, colors.accent))
                                    .child("AI writing help"),
                            )
                            .children(dialog.note.map(|note| {
                                div().p_2().rounded_md().bg(colors.selected).child(note)
                            }))
                            .child(div().text_color(colors.muted).child(
                                "\"Improve\" and \"Humanize\" send only the text you select, and \
                                 only when you ask, to OpenRouter, and ask it to use providers \
                                 that do not store or train on it. Nothing else from your notes \
                                 leaves this computer. The key is stored encrypted in your vault.",
                            ))
                            .child(label("OPENROUTER API KEY"))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(div().flex_1().child(dialog.key.clone()))
                                    .child(
                                        icon_button(
                                            "toggle-key",
                                            if dialog.show_key {
                                                Icon::EyeOff
                                            } else {
                                                Icon::Eye
                                            },
                                            false,
                                            if dialog.show_key {
                                                "Hide key"
                                            } else {
                                                "Show key"
                                            },
                                            cx,
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                if let Some(dialog) = this.ai_dialog.as_mut() {
                                                    dialog.show_key = !dialog.show_key;
                                                    let masked = !dialog.show_key;
                                                    dialog.key.update(cx, |f, cx| {
                                                        f.set_masked(masked, cx);
                                                    });
                                                    cx.notify();
                                                }
                                            }),
                                        ),
                                    ),
                            )
                            .child(
                                div().text_size(px(12.)).text_color(colors.muted).child(
                                    match &saved {
                                        Some(c) => {
                                            format!("A key ending in …{} is saved.", c.key_hint())
                                        }
                                        None => {
                                            "No key saved yet. Create one at openrouter.ai/keys."
                                                .to_owned()
                                        }
                                    },
                                ),
                            )
                            .child(label("MODEL"))
                            .child(model_cards)
                            .child(dialog.model.clone())
                            .child(div().text_size(px(12.)).text_color(colors.muted).child(
                                "The OpenRouter model id. Pick one above, or type any other.",
                            ))
                            .child(label("REASONING EFFORT"))
                            .child(effort_chips)
                            .child(div().text_size(px(12.)).text_color(colors.muted).child(
                                "How long the model thinks before it answers. Low or Medium is \
                                 plenty for editing; higher efforts take longer and cost more.",
                            ))
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .pt_2()
                                    .child(div().flex_1().when(saved.is_some(), |d| {
                                        d.child(
                                            button("remove-ai-key", "Remove key", false, cx)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.remove_ai_key(window, cx);
                                                })),
                                        )
                                    }))
                                    .child(button("cancel-ai", "Cancel", false, cx).on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.ai_dialog = None;
                                            window.focus(&this.focus_handle, cx);
                                            cx.notify();
                                        }),
                                    ))
                                    .child(button("save-ai", "Save", true, cx).on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.save_ai_settings(window, cx);
                                        }),
                                    )),
                            ),
                    ),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }
}
