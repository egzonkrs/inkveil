//! The root view: decides whether to show setup, unlock or the workspace.

use gpui::{
    Context, Entity, FocusHandle, Focusable, IntoElement, Render, SharedString, Subscription,
    Window, div, prelude::*, px,
};
use inkveil_store::{LockedVault, StoreError, Vault, VaultPaths};

use crate::{
    screens::{setup::SetupScreen, unlock::UnlockScreen},
    theme::{self, Theme, theme},
    workspace::Workspace,
};

enum Screen {
    Setup(Entity<SetupScreen>),
    Unlock(Entity<UnlockScreen>),
    Workspace(Entity<Workspace>),
    /// Something prevents the vault from opening at all.
    Message(SharedString),
    /// Locking in progress (pending saves are being written).
    Busy,
}

pub struct InkveilApp {
    paths: VaultPaths,
    screen: Screen,
    focus_handle: FocusHandle,
    _appearance: Subscription,
}

impl InkveilApp {
    pub fn new(paths: VaultPaths, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Theme::sync(window, cx);
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            Theme::sync(window, cx);
            cx.notify();
        });
        // Save before the window goes away; quitting alone runs too late to reach the views.
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            this.update(cx, |app, cx| app.flush_blocking(cx)).ok();
            true
        });
        let mut app = Self {
            paths,
            screen: Screen::Busy,
            focus_handle: cx.focus_handle(),
            _appearance: appearance,
        };
        app.open_vault(window, cx);
        app
    }

    /// Shows setup for a new vault, or the unlock screen for an existing one.
    fn open_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.paths.exists() && self.paths.has_vault_files() {
            // Offering setup here would put a new, empty vault over what is left of the old one.
            tracing::error!("the vault database is missing but its other files are there");
            self.screen = Screen::Message(
                format!(
                    "Inkveil cannot find your vault database (vault.db) in {}, but that folder \
                     still holds your vault's key header or backups. To keep them safe, Inkveil \
                     will not create a new vault there.\n\nIf vault.db was moved, renamed or put \
                     in quarantine, put it back, together with vault.db-wal if there is one (it \
                     holds the latest saves), and start Inkveil again. Full copies are in the \
                     newest folder of update-backups next to it (copy all its files), and older \
                     ones in backups (rename the newest vault-….db to vault.db).",
                    self.paths.dir().display()
                )
                .into(),
            );
            return;
        }
        if !self.paths.exists() {
            let app = cx.weak_entity();
            let paths = self.paths.clone();
            self.screen = Screen::Setup(cx.new(|cx| SetupScreen::new(app, paths, window, cx)));
            return;
        }
        match LockedVault::open(self.paths.clone()) {
            Ok(locked) => self.show_unlock(locked, window, cx),
            Err(StoreError::AlreadyOpen) => {
                self.screen = Screen::Message(
                    "Inkveil is already open in another window. Close it first.".into(),
                );
            }
            Err(error) => {
                tracing::error!("opening the vault failed: {error}");
                self.screen = Screen::Message(
                    format!(
                        "The vault could not be opened: {error}\n\nYour files are at {}",
                        self.paths.dir().display()
                    )
                    .into(),
                );
            }
        }
        cx.notify();
    }

    pub fn show_unlock(
        &mut self,
        locked: LockedVault,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app = cx.weak_entity();
        self.screen = Screen::Unlock(cx.new(|cx| UnlockScreen::new(app, locked, window, cx)));
        cx.notify();
    }

    pub fn show_workspace(&mut self, vault: Vault, window: &mut Window, cx: &mut Context<Self>) {
        let app = cx.weak_entity();
        self.screen = Screen::Workspace(cx.new(|cx| Workspace::new(app, vault, window, cx)));
        cx.notify();
    }

    /// Saves everything, wipes the keys and returns to the unlock screen.
    pub fn lock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Screen::Workspace(workspace) = std::mem::replace(&mut self.screen, Screen::Busy) else {
            return;
        };
        let storage = workspace.update(cx, super::workspace::Workspace::prepare_lock);
        drop(workspace);
        crate::images::ImageCache::clear(window, cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let locked = storage.lock().await;
            this.update_in(cx, |this, window, cx| match locked {
                Some(locked) => this.show_unlock(locked, window, cx),
                None => this.open_vault(window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Writes pending saves before the process exits. Returns once done or after a timeout.
    pub fn flush_blocking(&self, cx: &mut Context<Self>) {
        if let Screen::Workspace(workspace) = &self.screen {
            workspace.update(cx, super::workspace::Workspace::flush_blocking);
        }
    }
}

impl Focusable for InkveilApp {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for InkveilApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        let root = div()
            .size_full()
            .font_family(theme::UI_FONT)
            .bg(colors.background)
            .text_color(colors.text)
            .text_size(px(14.));
        match &self.screen {
            Screen::Setup(view) => root.child(view.clone()),
            Screen::Unlock(view) => root.child(view.clone()),
            Screen::Workspace(view) => root.child(view.clone()),
            Screen::Busy => root,
            Screen::Message(text) => root
                .flex()
                .items_center()
                .justify_center()
                .child(div().max_w(px(480.)).text_center().child(text.clone())),
        }
    }
}
