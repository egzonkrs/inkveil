//! Inkveil: private, local-only encrypted notes.

// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]
// GPUI's idiom is `&mut Context<Self>` and module-public items inside a binary; both lints only
// add noise here.
#![allow(elided_lifetimes_in_paths, unreachable_pub)]
// UI code: hex colors, pixel math on small counts, long declarative render functions and GPUI
// action handlers that must take `&mut self` read better without these pedantic lints.
#![allow(
    clippy::unreadable_literal,
    clippy::cast_precision_loss,
    clippy::too_many_lines,
    clippy::unused_self,
    clippy::ignored_unit_patterns
)]

mod ai;
mod app;
mod assets;
mod clipboard;
mod editor;
mod images;
mod screens;
mod search;
mod secrets;
mod settings;
mod spell;
mod storage;
mod theme;
mod ui;
mod workspace;

use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use gpui::{
    App, AppContext as _, Bounds, KeyBinding, WindowBounds, WindowOptions, actions, px, size,
};
use gpui_platform::application;
use inkveil_platform::Support;
use inkveil_store::VaultPaths;

actions!(inkveil, [Quit]);

static CAPTURE_PROTECTED: AtomicBool = AtomicBool::new(false);

/// Whether the main window is hidden from screen capture.
pub fn capture_protected() -> bool {
    CAPTURE_PROTECTED.load(Ordering::Relaxed)
}

fn main() {
    init_logging();
    install_panic_hook();
    spell::start_loading();

    match inkveil_platform::harden_process() {
        Ok(Support::Full) => {}
        Ok(Support::Unsupported) => {
            tracing::info!("process hardening not available on this platform");
        }
        Err(error) => tracing::warn!("process hardening failed: {error}"),
    }

    let paths = VaultPaths::new(vault_dir());
    application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            bind_keys(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());

            let bounds = initial_window_bounds(cx);
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Maximized(bounds)),
                        window_min_size: Some(size(px(640.), px(420.))),
                        titlebar: Some(gpui::TitlebarOptions {
                            title: Some("Inkveil".into()),
                            ..Default::default()
                        }),
                        app_id: Some("Inkveil".into()),
                        ..Default::default()
                    },
                    |window, cx| {
                        // Before anything is drawn, so no frame of content can be captured.
                        match exclude_from_capture(window) {
                            Ok(Support::Full) => CAPTURE_PROTECTED.store(true, Ordering::Relaxed),
                            Ok(Support::Unsupported) => {
                                tracing::warn!("screen capture exclusion is not supported here");
                            }
                            Err(error) => {
                                tracing::error!("screen capture exclusion failed: {error}");
                            }
                        }
                        if let Err(error) = inkveil_platform::configure_window(window) {
                            tracing::warn!("multi-monitor window configuration failed: {error}");
                        }
                        cx.new(|cx| app::InkveilApp::new(paths, window, cx))
                    },
                )
                .expect("failed to open the main window");

            // Write pending saves before the process exits, however it was asked to quit.
            cx.on_app_quit(move |cx| {
                window.update(cx, |app, _, cx| app.flush_blocking(cx)).ok();
                async {}
            })
            .detach();
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}

/// Computes a restored window size proportional to the primary display's usable work area, so
/// un-maximizing on a high-DPI laptop screen does not leave the window nearly full-screen.
fn initial_window_bounds(cx: &App) -> Bounds<gpui::Pixels> {
    let default_size = size(px(1180.), px(780.));
    let window_size = cx.primary_display().map_or(default_size, |display| {
        let visible = display.visible_bounds().size;
        let width = (visible.width * 0.74).clamp(px(640.), px(1180.)).round();
        let height = (visible.height * 0.76).clamp(px(420.), px(780.)).round();
        size(width, height)
    });
    Bounds::centered(None, window_size, cx)
}

fn exclude_from_capture(window: &gpui::Window) -> Result<Support, inkveil_platform::PlatformError> {
    // Debug builds can opt out so developers can take screenshots of their own test vault.
    #[cfg(debug_assertions)]
    if std::env::var_os("INKVEIL_DEV_ALLOW_CAPTURE").is_some() {
        tracing::warn!("screen capture exclusion disabled by INKVEIL_DEV_ALLOW_CAPTURE");
        return Ok(Support::Unsupported);
    }
    inkveil_platform::exclude_from_capture(window)
}

fn bind_keys(cx: &mut App) {
    cx.bind_keys(ui::text_field::key_bindings());
    cx.bind_keys(editor::actions::key_bindings());
    cx.bind_keys(workspace::key_bindings());
    cx.bind_keys([KeyBinding::new("alt-f4", Quit, None)]);
}

/// `%LOCALAPPDATA%\Inkveil\vault` on Windows (never the roaming profile, which may be synced).
///
/// Debug builds can point elsewhere with `INKVEIL_VAULT_DIR` for testing; release builds ignore it.
///
/// # Panics
///
/// If Windows reports no local app data folder for the user.
pub fn vault_dir() -> PathBuf {
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("INKVEIL_VAULT_DIR") {
        return PathBuf::from(dir);
    }
    let Some(base) = dirs::data_local_dir() else {
        // Without a per-user data folder there is no safe place for the vault.
        panic!("Windows did not report a local app data folder for this user");
    };
    base.join("Inkveil").join("vault")
}

/// Logs go to stderr in debug builds only, and never contain note content.
fn init_logging() {
    #[cfg(debug_assertions)]
    {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new(
                "inkveil=info,inkveil_store=info,inkveil_platform=info,warn",
            )
        });
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
    }
}

/// On a crash, give queued saves a moment to reach the disk before the process dies.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(storage) = storage::active() {
            storage.wait_idle_blocking(Duration::from_secs(2));
        }
        default(info);
    }));
}
