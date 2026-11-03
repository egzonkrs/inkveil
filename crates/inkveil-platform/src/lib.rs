//! Operating system integration for Inkveil.
//!
//! This is the only crate allowed to use `unsafe`, because talking to the operating system
//! requires it. Every unsafe block states why it is sound.
//!
//! - [`exclude_from_capture`] hides a window from screenshots, recordings and screen sharing.
//! - [`configure_window`] fixes window sizing across monitors with different scaling.
//! - [`harden_process`] stops other non-admin processes from reading Inkveil's memory.
//! - [`write_private_clipboard_text`] copies text without it entering clipboard history or sync.
//!
//! Each function reports what it could do, so the app can tell the user honestly when a platform
//! does not support something.

use raw_window_handle::HasWindowHandle;
use thiserror::Error;

#[cfg(windows)]
mod windows;

/// An operating system call failed.
#[derive(Debug, Error)]
#[error("{operation} failed: {message}")]
pub struct PlatformError {
    operation: &'static str,
    message: String,
}

impl PlatformError {
    #[cfg_attr(not(windows), allow(dead_code))]
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn new(operation: &'static str, message: impl ToString) -> Self {
        Self {
            operation,
            message: message.to_string(),
        }
    }
}

/// How well a protection could be applied on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Fully applied.
    Full,
    /// This platform has no way to do it.
    Unsupported,
}

/// Hides `window` from screen capture: screenshots, recording software such as OBS, screen
/// sharing and, on Windows, Recall. Captures show the area as blank.
///
/// On Windows 10 version 2004 and later this is complete. Elsewhere it returns
/// [`Support::Unsupported`] for now.
pub fn exclude_from_capture(window: &impl HasWindowHandle) -> Result<Support, PlatformError> {
    #[cfg(windows)]
    {
        windows::exclude_from_capture(window)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Ok(Support::Unsupported)
    }
}

/// Fixes window sizing across monitors with different sizes and scaling: after the user drags
/// `window` to another monitor it keeps its share of the screen (instead of nearly filling a
/// small high-DPI laptop screen), a window maximized on another monitor gets a restore size that
/// fits there, and a maximized window keeps its status bar on screen after a DPI change.
pub fn configure_window(window: &impl HasWindowHandle) -> Result<Support, PlatformError> {
    #[cfg(windows)]
    {
        windows::configure_window(window)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Ok(Support::Unsupported)
    }
}

/// Hardens this process against other programs: DLLs load from System32 only, legacy injection
/// points (`AppInit` DLLs, global hooks) are disabled, crash dumps leave out the heap, and other
/// non-admin programs cannot open the process to read its memory or start threads in it.
///
/// Call first thing at startup. Administrators and the operating system can still access the
/// process; see SECURITY.md for what remains possible for malware running as the same user.
pub fn harden_process() -> Result<Support, PlatformError> {
    #[cfg(windows)]
    {
        windows::harden_process()
    }
    #[cfg(not(windows))]
    {
        Ok(Support::Unsupported)
    }
}

/// Whether the user's desktop session is locked (Win+L, lock screen, screen saver lock).
///
/// Returns `None` where this cannot be determined.
pub fn is_session_locked() -> Option<bool> {
    #[cfg(windows)]
    {
        windows::is_session_locked()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Puts `text` on the clipboard, marked so that clipboard history, cloud clipboard sync and
/// clipboard monitoring tools skip it.
///
/// Returns the clipboard sequence number after the write (to clear it later only if it is still
/// ours), or `None` when this platform has no such marking; the caller should then use its normal
/// clipboard path. On error the clipboard is left empty, never holding unmarked text.
pub fn write_private_clipboard_text(text: &str) -> Result<Option<u32>, PlatformError> {
    #[cfg(windows)]
    {
        windows::write_private_clipboard_text(text).map(Some)
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        Ok(None)
    }
}

/// Puts a picture on the clipboard (as PNG and as a bitmap), marked like
/// [`write_private_clipboard_text`]. `dib` is a BITMAPINFOHEADER followed by the pixels.
pub fn write_private_clipboard_image(png: &[u8], dib: &[u8]) -> Result<Option<u32>, PlatformError> {
    #[cfg(windows)]
    {
        windows::write_private_clipboard_image(png, dib).map(Some)
    }
    #[cfg(not(windows))]
    {
        let _ = (png, dib);
        Ok(None)
    }
}

/// Empties the clipboard if its sequence number is still `sequence`, i.e. nothing was copied
/// since Inkveil's own write.
pub fn clear_clipboard_if_unchanged(sequence: u32) -> Result<bool, PlatformError> {
    #[cfg(windows)]
    {
        if windows::clipboard_sequence() != sequence {
            return Ok(false);
        }
        windows::clear_clipboard().map(|()| true)
    }
    #[cfg(not(windows))]
    {
        let _ = sequence;
        Ok(false)
    }
}
