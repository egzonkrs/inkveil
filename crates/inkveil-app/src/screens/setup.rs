//! First run: choose a password, then save the recovery key.

use std::time::Duration;

use gpui::{
    Context, Entity, Focusable, IntoElement, Render, SharedString, WeakEntity, Window, div,
    prelude::*, px,
};
use inkveil_crypto::{KdfParams, MIN_PASSWORD_CHARS, Password, PasswordPolicyError};
use inkveil_store::{Vault, VaultPaths};
use zeroize::Zeroizing;

use crate::{
    app::InkveilApp,
    screens::{brand, centered_card},
    theme::{self, theme},
    ui::{
        button,
        text_field::{TextField, TextFieldEvent},
    },
};

/// How long unlocking should take on this machine. Slower means stronger against guessing.
const UNLOCK_TIME: Duration = Duration::from_millis(800);

enum Step {
    ChoosePassword,
    Creating,
    /// The vault exists; the recovery key is shown once.
    SaveRecoveryKey {
        vault: Box<Vault>,
        key: Zeroizing<String>,
    },
}

pub struct SetupScreen {
    app: WeakEntity<InkveilApp>,
    paths: VaultPaths,
    password: Entity<TextField>,
    confirm: Entity<TextField>,
    step: Step,
    error: Option<SharedString>,
    copied: bool,
    /// Focused on the recovery key step, so Enter continues.
    focus_handle: gpui::FocusHandle,
    pending_focus: bool,
}

impl SetupScreen {
    pub fn new(
        app: WeakEntity<InkveilApp>,
        paths: VaultPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let password = cx.new(|cx| TextField::new("Password", window, cx).masked());
        let confirm = cx.new(|cx| TextField::new("Repeat password", window, cx).masked());
        for field in [&password, &confirm] {
            cx.subscribe_in(field, window, |this, _, event, window, cx| match event {
                TextFieldEvent::Submitted => this.create(window, cx),
                TextFieldEvent::Changed => {
                    this.error = None;
                    cx.notify();
                }
                TextFieldEvent::Blurred => {}
            })
            .detach();
        }
        window.focus(&password.focus_handle(cx), cx);
        Self {
            app,
            paths,
            password,
            confirm,
            step: Step::ChoosePassword,
            error: None,
            copied: false,
            focus_handle: cx.focus_handle(),
            pending_focus: false,
        }
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.step, Step::ChoosePassword) {
            return;
        }
        let password = Password::new(self.password.read(cx).text());
        if let Err(problem) = password.check_policy() {
            self.error = Some(problem.to_string().into());
            cx.notify();
            return;
        }
        if self.password.read(cx).text() != self.confirm.read(cx).text() {
            self.error = Some("The passwords do not match.".into());
            cx.notify();
            return;
        }
        self.step = Step::Creating;
        cx.notify();

        let paths = self.paths.clone();
        let task = cx.background_spawn(async move {
            let kdf = KdfParams::calibrate(UNLOCK_TIME)?;
            let mut vault = Vault::create(paths, &password, kdf)?;
            // The vault exists now; a failed recovery key must not strand the user on this
            // screen (a retry would fail with "already exists"). They can make one later.
            let key = match vault.create_recovery_key() {
                Ok(key) => key.display(),
                Err(error) => {
                    tracing::error!("creating the recovery key failed: {error}");
                    Zeroizing::new(String::new())
                }
            };
            Ok::<_, inkveil_store::StoreError>((vault, key))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok((vault, key)) => {
                        this.step = Step::SaveRecoveryKey {
                            vault: Box::new(vault),
                            key,
                        };
                        this.pending_focus = true;
                    }
                    Err(error) => {
                        tracing::error!("creating the vault failed: {error}");
                        this.error = Some(format!("Could not create the vault: {error}").into());
                        this.step = Step::ChoosePassword;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A copied recovery key stays on the clipboard for a while after continuing, so it can
        // still be pasted into a password manager; it is removed on its own.
        let step = std::mem::replace(&mut self.step, Step::Creating);
        if let Step::SaveRecoveryKey { vault, .. } = step {
            self.app
                .update(cx, |app, cx| app.show_workspace(*vault, window, cx))
                .ok();
        }
    }

    fn render_password_step(&self, cx: &mut Context<Self>) -> gpui::Div {
        let colors = theme(cx);
        let creating = matches!(self.step, Step::Creating);
        let text = self.password.read(cx).text();
        let policy = Password::new(text).check_policy();
        let long_enough = !matches!(policy, Err(PasswordPolicyError::TooShort { .. }));
        let has_number = text.chars().any(char::is_numeric);
        let rule = |ok: bool, label: String| {
            div()
                .flex()
                .gap_2()
                .text_size(px(13.))
                .text_color(if ok { colors.text } else { colors.muted })
                .child(if ok { "✓" } else { "•" })
                .child(label)
        };
        div()
            .w(px(340.))
            .flex()
            .flex_col()
            .gap_4()
            .child(brand(cx))
            .child(
                div()
                    .text_center()
                    .text_color(colors.muted)
                    .child("Choose a password. It encrypts every note on this device."),
            )
            .child(self.password.clone())
            .child(self.confirm.clone())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(rule(
                        long_enough && !text.is_empty(),
                        format!("At least {MIN_PASSWORD_CHARS} characters"),
                    ))
                    .child(rule(has_number, "At least one number".into())),
            )
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_color(colors.error).text_size(px(13.)).child(e)),
            )
            .child(
                button(
                    "create",
                    if creating { "Creating your vault…" } else { "Create vault" },
                    true,
                    cx,
                )
                .w_full()
                .when(!creating, |b| {
                    b.on_click(cx.listener(|this, _, window, cx| this.create(window, cx)))
                }),
            )
            .child(
                div()
                    .text_center()
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .child("There is no way to recover a forgotten password except the recovery key shown next."),
            )
    }

    fn render_recovery_step(&self, key: &str, cx: &mut Context<Self>) -> gpui::Div {
        let colors = theme(cx);
        div()
            .w(px(500.))
            .flex()
            .flex_col()
            .gap_4()
            .child(brand(cx))
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_center()
                    .child("Save your recovery key"),
            )
            .child(
                div()
                    .text_color(colors.muted)
                    .text_center()
                    .child("If you forget your password, this key is the only way back into your notes. Write it down or print it and keep it somewhere safe. It is shown only now."),
            )
            .child(
                div()
                    .p_4()
                    .rounded_md()
                    .bg(colors.code_background)
                    .font_family(theme::MONO_FONT)
                    .text_size(px(16.))
                    .text_center()
                    .whitespace_nowrap()
                    .child(key.to_owned()),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button("copy-key", if self.copied { "Copied" } else { "Copy" }, false, cx)
                            .flex_1()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Step::SaveRecoveryKey { key, .. } = &this.step {
                                    crate::clipboard::write_secret_text(key, cx);
                                    this.copied = true;
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        button("saved-key", "I saved it, continue", true, cx)
                            .flex_1()
                            .on_click(cx.listener(|this, _, window, cx| this.finish(window, cx))),
                    ),
            )
            .when(self.copied, |d| {
                d.child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.muted)
                        .text_center()
                        .child(
                            "Copied. Paste it with Ctrl+V. For safety it is kept out of \
                             clipboard history (Win+V) and removed from the clipboard after 2 \
                             minutes.",
                        ),
                )
            })
    }
}

impl Render for SetupScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.step {
            Step::SaveRecoveryKey { key, .. } if key.is_empty() => div()
                .w(px(420.))
                .flex()
                .flex_col()
                .gap_4()
                .child(brand(cx))
                .child(div().text_center().child(
                    "Your vault is ready, but a recovery key could not be created. \
                         Create one from the settings menu (bottom left) as soon as possible.",
                ))
                .child(
                    button("continue-without-key", "Continue", true, cx)
                        .on_click(cx.listener(|this, _, window, cx| this.finish(window, cx))),
                ),
            Step::SaveRecoveryKey { key, .. } => {
                let key = key.to_string();
                self.render_recovery_step(&key, cx)
            }
            _ => self.render_password_step(cx),
        };
        if std::mem::take(&mut self.pending_focus) {
            window.focus(&self.focus_handle, cx);
        }
        centered_card()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "enter"
                    && matches!(this.step, Step::SaveRecoveryKey { .. })
                {
                    this.finish(window, cx);
                }
            }))
            .child(content)
    }
}
