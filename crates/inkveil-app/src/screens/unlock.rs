//! The unlock screen.

use std::time::{Duration, Instant};

use gpui::{
    Context, Entity, Focusable, IntoElement, Render, SharedString, Task, WeakEntity, Window, div,
    prelude::*, px,
};
use inkveil_crypto::{Password, RecoveryKey};
use inkveil_store::{Credential, FREE_ATTEMPTS, LockedVault, StoreError, wait_after};

use crate::{
    app::InkveilApp,
    screens::{brand, centered_card},
    theme::theme,
    ui::{
        button,
        text_field::{TextField, TextFieldEvent},
    },
};

enum Secret {
    Password(Password),
    Recovery(RecoveryKey),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Password,
    RecoveryKey,
}

pub struct UnlockScreen {
    app: WeakEntity<InkveilApp>,
    /// `None` while an unlock attempt is running on a background thread.
    locked: Option<LockedVault>,
    field: Entity<TextField>,
    mode: Mode,
    error: Option<SharedString>,
    /// Unlocking is paused after too many wrong attempts until then.
    paused_until: Option<Instant>,
    /// Redraws the countdown while paused.
    countdown: Option<Task<()>>,
}

impl UnlockScreen {
    pub fn new(
        app: WeakEntity<InkveilApp>,
        locked: LockedVault,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = cx.new(|cx| TextField::new("Password", window, cx).masked());
        cx.subscribe_in(&field, window, |this, _, event, window, cx| match event {
            TextFieldEvent::Submitted => this.unlock(window, cx),
            TextFieldEvent::Changed => {
                this.error = None;
                cx.notify();
            }
            TextFieldEvent::Blurred => {}
        })
        .detach();
        window.focus(&field.focus_handle(cx), cx);

        // Debug builds only: unlock a test vault without typing, for automated UI checks.
        #[cfg(debug_assertions)]
        if let Ok(password) = std::env::var("INKVEIL_DEV_PASSWORD") {
            field.update(cx, |f, cx| f.set_text(&password, cx));
            cx.defer_in(window, |this: &mut Self, window, cx| {
                this.unlock(window, cx);
            });
        }

        let wait = locked.unlock_wait();
        let mut this = Self {
            app,
            locked: Some(locked),
            field,
            mode: Mode::Password,
            error: None,
            paused_until: None,
            countdown: None,
        };
        if let Some(wait) = wait {
            this.pause(wait, window, cx);
        }
        this
    }

    /// Pauses unlocking for `wait`, redrawing the countdown twice a second.
    fn pause(&mut self, wait: Duration, window: &mut Window, cx: &mut Context<Self>) {
        self.paused_until = Some(Instant::now() + wait);
        let task = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let done = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        let over = this.paused_until.is_none_or(|t| Instant::now() >= t);
                        if over {
                            this.paused_until = None;
                            this.error = None;
                        }
                        over
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        });
        // Replacing the task stops the previous countdown.
        drop(self.countdown.replace(task));
        cx.notify();
    }

    fn pause_left(&self) -> Option<Duration> {
        self.paused_until
            .map(|t| t.saturating_duration_since(Instant::now()))
            .filter(|d| !d.is_zero())
    }

    fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        self.error = None;
        let placeholder = match mode {
            Mode::Password => "Password",
            Mode::RecoveryKey => "Recovery key, e.g. 7K3M-Q9TD-…",
        };
        self.field = cx.new(|cx| {
            let field = TextField::new(placeholder, window, cx);
            if mode == Mode::Password {
                field.masked()
            } else {
                field
            }
        });
        cx.subscribe_in(&self.field, window, |this, _, event, window, cx| {
            if *event == TextFieldEvent::Submitted {
                this.unlock(window, cx);
            }
        })
        .detach();
        window.focus(&self.field.focus_handle(cx), cx);
        cx.notify();
    }

    fn unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pause_left().is_some() {
            return;
        }
        let Some(locked) = self.locked.take() else {
            return;
        };
        let secret = match self.mode {
            Mode::Password => Secret::Password(Password::new(self.field.read(cx).text())),
            Mode::RecoveryKey => match RecoveryKey::parse(self.field.read(cx).text()) {
                Ok(key) => Secret::Recovery(key),
                Err(error) => {
                    self.locked = Some(locked);
                    self.error = Some(error.to_string().into());
                    cx.notify();
                    return;
                }
            },
        };
        self.error = None;
        cx.notify();

        // Argon2id takes about a second by design, so it runs off the UI thread.
        let attempt = cx.background_spawn(async move {
            match &secret {
                Secret::Password(password) => locked.unlock(Credential::Password(password)),
                Secret::Recovery(key) => locked.unlock(Credential::RecoveryKey(key)),
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = attempt.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(vault) => {
                    this.app
                        .update(cx, |app, cx| app.show_workspace(vault, window, cx))
                        .ok();
                }
                Err(failure) => {
                    let failure = *failure;
                    let failures = failure.vault.failed_attempts().failures;
                    let wrong = match this.mode {
                        Mode::Password => "Wrong password.",
                        Mode::RecoveryKey => "Wrong recovery key.",
                    };
                    this.error = Some(match failure.error {
                        StoreError::WrongCredential if !wait_after(failures).is_zero() => {
                            this.pause(wait_after(failures), window, cx);
                            wrong.into()
                        }
                        // Warn only when the pause is close.
                        StoreError::WrongCredential if failures + 3 > FREE_ATTEMPTS => {
                            let left = FREE_ATTEMPTS + 1 - failures;
                            format!(
                                "{wrong} After {left} more wrong attempt{}, unlocking pauses for {}.",
                                if left == 1 { "" } else { "s" },
                                describe(wait_after(FREE_ATTEMPTS + 1))
                            )
                            .into()
                        }
                        StoreError::WrongCredential => wrong.into(),
                        StoreError::TooManyAttempts(wait) => {
                            this.pause(wait, window, cx);
                            "Too many wrong attempts.".into()
                        }
                        StoreError::NoSuchSlot => "This vault has no recovery key.".into(),
                        other => other.to_string().into(),
                    });
                    this.locked = Some(failure.vault);
                    this.field
                        .update(cx, super::super::ui::text_field::TextField::clear);
                    window.focus(&this.field.focus_handle(cx), cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}

/// "30 seconds", "5 minutes", "1 hour".
fn describe(wait: Duration) -> String {
    let secs = wait.as_secs();
    match secs {
        0..60 => format!("{secs} seconds"),
        60..120 => "1 minute".into(),
        120..3600 => format!("{} minutes", secs / 60),
        3600..7200 => "1 hour".into(),
        _ => format!("{} hours", secs / 3600),
    }
}

/// A countdown like "4:59" or "1:00:00".
fn countdown(left: Duration) -> String {
    let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

impl Render for UnlockScreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        let busy = self.locked.is_none();
        let paused = self.pause_left();
        let has_recovery = self
            .locked
            .as_ref()
            .is_some_and(|l| l.header().has_recovery_key());
        let (switch_label, switch_to) = match self.mode {
            Mode::Password => ("Use recovery key instead", Mode::RecoveryKey),
            Mode::RecoveryKey => ("Use password instead", Mode::Password),
        };
        centered_card().child(
            div()
                .w(px(320.))
                .flex()
                .flex_col()
                .gap_4()
                .child(brand(cx))
                .child(
                    div()
                        .text_center()
                        .text_color(colors.muted)
                        .child("Your notes are locked."),
                )
                .child(self.field.clone())
                .children(
                    self.error
                        .clone()
                        .map(|e| div().text_color(colors.error).text_size(px(13.)).child(e)),
                )
                .children(paused.map(|left| {
                    div()
                        .text_color(colors.muted)
                        .text_size(px(13.))
                        .child(format!(
                            "To slow down password guessing, unlocking is paused. Try again in {}.",
                            countdown(left)
                        ))
                }))
                .child(
                    button(
                        "unlock",
                        match (busy, paused) {
                            (true, _) => "Unlocking…".to_owned(),
                            (false, Some(left)) => format!("Try again in {}", countdown(left)),
                            (false, None) => "Unlock".to_owned(),
                        },
                        true,
                        cx,
                    )
                    .w_full()
                    .when(paused.is_some(), |b| b.opacity(0.5).cursor_default())
                    .when(!busy && paused.is_none(), |b| {
                        b.on_click(cx.listener(|this, _, window, cx| this.unlock(window, cx)))
                    }),
                )
                .when(has_recovery || self.mode == Mode::RecoveryKey, |d| {
                    d.child(
                        div()
                            .id("switch-mode")
                            .text_center()
                            .text_size(px(13.))
                            .text_color(colors.accent)
                            .cursor_pointer()
                            .child(switch_label)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_mode(switch_to, window, cx);
                            })),
                    )
                }),
        )
    }
}
