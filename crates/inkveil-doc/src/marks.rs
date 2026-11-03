use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A 24-bit sRGB color, `0xRRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgb(pub u32);

/// Inline formatting of a run of text.
// Each flag is an independent, user-visible toggle; an enum set would only obscure that.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Marks {
    /// Bold weight.
    pub bold: bool,
    /// Italic style.
    pub italic: bool,
    /// Underlined.
    pub underline: bool,
    /// Struck through.
    pub strike: bool,
    /// Inline code (monospace font).
    pub code: bool,
    /// Text color; `None` uses the theme color.
    pub color: Option<Rgb>,
    /// Highlight (background) color.
    pub highlight: Option<Rgb>,
    /// Link target.
    pub link: Option<Arc<str>>,
}

impl Marks {
    /// Plain text with no formatting.
    pub const NONE: Self = Self {
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        code: false,
        color: None,
        highlight: None,
        link: None,
    };

    /// Whether no mark is set.
    pub fn is_plain(&self) -> bool {
        *self == Self::NONE
    }
}
