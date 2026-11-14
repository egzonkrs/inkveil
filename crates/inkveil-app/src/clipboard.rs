//! Clipboard writes that keep note text out of clipboard history and sync, and are cleared when
//! the vault locks or the app quits.

use std::sync::Mutex;

use gpui::{App, ClipboardItem};

/// Sequence number of Inkveil's last clipboard write, if it may still be on the clipboard.
static LAST_WRITE: Mutex<Option<u32>> = Mutex::new(None);

/// How long a copied secret (a recovery key) stays on the clipboard.
pub const SECRET_LIFETIME: std::time::Duration = std::time::Duration::from_secs(120);

/// What to tell the user when a copy did not reach the clipboard.
pub const BUSY: &str = "Could not copy: another program is using the clipboard. Try again.";

/// Copies `text`, marked as private where the platform supports it. Returns whether it reached
/// the clipboard: another program can hold the clipboard open, and then nothing is copied.
pub fn write_text(text: &str, cx: &mut App) -> bool {
    write(text, cx).is_ok()
}

/// Copies `text` and returns the clipboard sequence number of the write, if known.
fn write(text: &str, cx: &mut App) -> Result<Option<u32>, ()> {
    // Tests must not overwrite the clipboard of whoever runs them.
    if cfg!(test) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        return Ok(None);
    }
    match inkveil_platform::write_private_clipboard_text(text) {
        Ok(Some(sequence)) => {
            if let Ok(mut last) = LAST_WRITE.lock() {
                *last = Some(sequence);
            }
            Ok(Some(sequence))
        }
        Ok(None) => {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
            Ok(None)
        }
        // The clipboard was left empty rather than holding unmarked text.
        Err(error) => {
            tracing::warn!("private clipboard write failed: {error}");
            Err(())
        }
    }
}

/// Copies a secret (a recovery key) and removes it from the clipboard after
/// [`SECRET_LIFETIME`], unless something else was copied in the meantime.
pub fn write_secret_text(text: &str, cx: &mut App) {
    let Ok(Some(sequence)) = write(text, cx) else {
        return;
    };
    cx.spawn(async move |cx| {
        cx.background_executor().timer(SECRET_LIFETIME).await;
        if let Err(error) = inkveil_platform::clear_clipboard_if_unchanged(sequence) {
            tracing::warn!("could not clear the clipboard: {error}");
        }
    })
    .detach();
}

/// Copies a picture (PNG and bitmap), marked as private like text. Where the platform has no
/// private clipboard, nothing is copied rather than an unmarked picture.
pub fn write_image(png: &[u8], dib: &[u8]) {
    match inkveil_platform::write_private_clipboard_image(png, dib) {
        Ok(Some(sequence)) => {
            if let Ok(mut last) = LAST_WRITE.lock() {
                *last = Some(sequence);
            }
        }
        Ok(None) => tracing::warn!("copying pictures is not supported on this platform yet"),
        Err(error) => tracing::warn!("private clipboard write failed: {error}"),
    }
}

/// Clears the clipboard if it still holds what Inkveil last put there.
pub fn clear_ours() {
    let Some(sequence) = LAST_WRITE.lock().ok().and_then(|mut last| last.take()) else {
        return;
    };
    if let Err(error) = inkveil_platform::clear_clipboard_if_unchanged(sequence) {
        tracing::warn!("could not clear the clipboard: {error}");
    }
}
