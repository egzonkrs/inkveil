//! Colors and fonts, following the system's light or dark appearance.

use gpui::{App, Global, Hsla, Window, WindowAppearance, rgb, rgba};

/// UI font family.
pub const UI_FONT: &str = "Segoe UI";
/// Monospace font family for code.
pub const MONO_FONT: &str = "Cascadia Mono";

/// The active color palette.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub dark: bool,
    /// Editor and main area background.
    pub background: Hsla,
    /// Sidebar and tab strip background.
    pub surface: Hsla,
    /// Hover background for list rows and buttons.
    pub hover: Hsla,
    /// Selected row / active tab background.
    pub selected: Hsla,
    /// Thin separators.
    pub border: Hsla,
    /// Body text.
    pub text: Hsla,
    /// Secondary text (dates, placeholders, inactive tabs).
    pub muted: Hsla,
    /// Icons in toolbars.
    pub icon: Hsla,
    /// Accent for focus rings, active toggles and links.
    pub accent: Hsla,
    /// Text selection background.
    pub selection: Hsla,
    /// Caret color.
    pub caret: Hsla,
    /// Code background.
    pub code_background: Hsla,
    /// Errors (validation, spelling).
    pub error: Hsla,
    /// Input field background.
    pub input: Hsla,
    /// Find results in the editor.
    pub find_match: Hsla,
    /// The find result the caret is on.
    pub find_current: Hsla,
}

impl Theme {
    pub fn light() -> Self {
        Self {
            dark: false,
            background: rgb(0xffffff).into(),
            surface: rgb(0xf7f7f8).into(),
            hover: rgb(0xececee).into(),
            selected: rgb(0xe4e6ea).into(),
            border: rgb(0xe3e4e8).into(),
            text: rgb(0x1f2328).into(),
            muted: rgb(0x8a8f98).into(),
            icon: rgb(0x4b5058).into(),
            accent: rgb(0x2f6fdb).into(),
            selection: rgba(0x2f6fdb40).into(),
            caret: rgb(0x1f2328).into(),
            code_background: rgb(0xf0f1f3).into(),
            error: rgb(0xd1242f).into(),
            input: rgb(0xffffff).into(),
            find_match: rgba(0xf5c34a59).into(),
            find_current: rgba(0xf59e0bb3).into(),
        }
    }

    pub fn dark() -> Self {
        Self {
            dark: true,
            background: rgb(0x1f1f1f).into(),
            surface: rgb(0x181818).into(),
            hover: rgb(0x2a2d2e).into(),
            selected: rgb(0x37373d).into(),
            border: rgb(0x2b2b2b).into(),
            text: rgb(0xcccccc).into(),
            muted: rgb(0x8b8b8b).into(),
            icon: rgb(0xc5c5c5).into(),
            accent: rgb(0x4d9cf8).into(),
            selection: rgba(0x264f78cc).into(),
            caret: rgb(0xaeafad).into(),
            code_background: rgb(0x2a2a2a).into(),
            error: rgb(0xf14c4c).into(),
            input: rgb(0x252526).into(),
            find_match: rgba(0xea5c0055).into(),
            find_current: rgba(0x9e6a03e6).into(),
        }
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
        }
    }

    /// Picks the theme for `window` and makes it current.
    pub fn sync(window: &Window, cx: &mut App) {
        cx.set_global(Self::for_appearance(window.appearance()));
    }
}

impl Global for Theme {}

/// The current theme.
pub fn theme(cx: &App) -> Theme {
    cx.try_global::<Theme>()
        .copied()
        .unwrap_or_else(Theme::light)
}
