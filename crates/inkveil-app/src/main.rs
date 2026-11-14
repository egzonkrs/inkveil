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

            let remembered = remembered_window();
            let bounds = initial_window_bounds(remembered.map(|(_, size)| size), cx);
            let window = cx
                .open_window(
                    WindowOptions {
                        // As it was closed: maximized, or at its own size. Maximized the first
                        // time.
                        window_bounds: Some(match remembered {
                            Some((false, _)) => WindowBounds::Windowed(bounds),
                            _ => WindowBounds::Maximized(bounds),
                        }),
                        window_min_size: Some(size(px(MIN_WINDOW.0), px(MIN_WINDOW.1))),
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
                window
                    .update(cx, |app, window, cx| {
                        remember_window(window.window_bounds());
                        app.flush_blocking(cx);
                    })
                    .ok();
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

/// The smallest the window can be made.
const MIN_WINDOW: (f32, f32) = (640., 420.);

/// Computes a restored window size proportional to the primary display's usable work area, so
/// un-maximizing on a high-DPI laptop screen does not leave the window nearly full-screen.
/// With a `remembered` size, that is used instead, cut to what fits on the display. The window
/// is always centered: only its size is remembered, so it can never open off screen.
fn initial_window_bounds(
    remembered: Option<gpui::Size<gpui::Pixels>>,
    cx: &App,
) -> Bounds<gpui::Pixels> {
    let default_size = size(px(1180.), px(780.));
    let (min_width, min_height) = (px(MIN_WINDOW.0), px(MIN_WINDOW.1));
    let window_size = cx.primary_display().map_or(default_size, |display| {
        let visible = display.visible_bounds().size;
        match remembered {
            Some(remembered) => size(
                remembered.width.min(visible.width).max(min_width).round(),
                remembered
                    .height
                    .min(visible.height)
                    .max(min_height)
                    .round(),
            ),
            None => size(
                (visible.width * 0.74).clamp(min_width, px(1180.)).round(),
                (visible.height * 0.76).clamp(min_height, px(780.)).round(),
            ),
        }
    });
    Bounds::centered(None, window_size, cx)
}

/// Where the window's state is kept: beside the vault folder, not in it, so the vault's files
/// only change when notes do. It holds two numbers and a word, nothing about any note.
fn window_state_path() -> Option<PathBuf> {
    Some(vault_dir().parent()?.join("window-state"))
}

/// The window state as text: "maximized" or "windowed", then the width and height it has when
/// it is not maximized.
fn window_state_text(bounds: WindowBounds) -> String {
    let (word, bounds) = match bounds {
        WindowBounds::Windowed(bounds) => ("windowed", bounds),
        WindowBounds::Maximized(bounds) | WindowBounds::Fullscreen(bounds) => ("maximized", bounds),
    };
    format!(
        "{word} {} {}",
        bounds.size.width.as_f32().round(),
        bounds.size.height.as_f32().round()
    )
}

/// Reads [`window_state_text`] back: whether the window was maximized, and its size when it is
/// not. Anything unexpected reads as nothing remembered.
fn parse_window_state(text: &str) -> Option<(bool, gpui::Size<gpui::Pixels>)> {
    let mut parts = text.split_whitespace();
    let maximized = match parts.next()? {
        "maximized" => true,
        "windowed" => false,
        _ => return None,
    };
    let mut number = || {
        let value = parts.next()?.parse::<f32>().ok()?;
        (value.is_finite() && (100. ..=100_000.).contains(&value)).then_some(px(value))
    };
    let (width, height) = (number()?, number()?);
    Some((maximized, size(width, height)))
}

fn remembered_window() -> Option<(bool, gpui::Size<gpui::Pixels>)> {
    parse_window_state(&std::fs::read_to_string(window_state_path()?).ok()?)
}

/// Remembers how the window was when it closed, for the next start. Failing to is harmless.
pub fn remember_window(bounds: WindowBounds) {
    if let Some(path) = window_state_path()
        && let Err(error) = std::fs::write(path, window_state_text(bounds))
    {
        tracing::warn!("could not remember the window size: {error}");
    }
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
        // Not when it is the storage thread that crashed: it would wait for itself.
        let storage_crashed = std::thread::current().name() == Some(storage::THREAD_NAME);
        if !storage_crashed && let Some(storage) = storage::active() {
            storage.wait_idle_blocking(Duration::from_secs(2));
        }
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    use gpui::point;

    use super::*;

    #[test]
    fn the_window_state_reads_back_as_it_was_written() {
        let bounds = Bounds::new(point(px(40.), px(60.)), size(px(900.4), px(700.)));
        let text = window_state_text(WindowBounds::Windowed(bounds));
        assert_eq!(text, "windowed 900 700");
        assert_eq!(
            parse_window_state(&text),
            Some((false, size(px(900.), px(700.))))
        );
        let text = window_state_text(WindowBounds::Maximized(bounds));
        assert_eq!(
            parse_window_state(&text),
            Some((true, size(px(900.), px(700.))))
        );
    }

    #[test]
    fn a_damaged_window_state_is_ignored() {
        for text in [
            "",
            "sideways 900 700",
            "windowed 900",
            "windowed NaN 700",
            "windowed 5 700",
            "windowed 900 inf",
        ] {
            assert_eq!(parse_window_state(text), None, "{text:?}");
        }
    }
}
