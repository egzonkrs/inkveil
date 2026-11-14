//! Icons compiled into the binary, so the app never loads anything from disk or network.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$(
            (
                concat!("icons/", $name, ".svg"),
                include_bytes!(concat!("../assets/icons/", $name, ".svg")),
            ),
        )*]
    };
}

/// Lucide icons (ISC license, see `assets/icons/LICENSE-lucide.txt`).
const ICONS: &[(&str, &[u8])] = icons![
    "a-large-small",
    "align-center",
    "align-left",
    "align-right",
    "arrow-down",
    "arrow-left-right",
    "arrow-up",
    "baseline",
    "bold",
    "case-sensitive",
    "check",
    "chevron-down",
    "chevron-right",
    "code",
    "columns-2",
    "copy",
    "dollar-sign",
    "ellipsis",
    "eye",
    "eye-off",
    "file-text",
    "folder",
    "folder-plus",
    "grip-horizontal",
    "grok",
    "heading",
    "highlighter",
    "history",
    "image",
    "italic",
    "link",
    "list",
    "list-checks",
    "list-indent-decrease",
    "list-indent-increase",
    "list-ordered",
    "lock",
    "message-circle",
    "minus",
    "notebook-pen",
    "pilcrow",
    "pin",
    "plus",
    "quote",
    "redo-2",
    "remove-formatting",
    "replace",
    "replace-all",
    "rotate-ccw",
    "rows-2",
    "search",
    "settings",
    "shield-check",
    "sparkles",
    "square",
    "square-check-big",
    "square-code",
    "strikethrough",
    "table",
    "trash-2",
    "type",
    "underline",
    "undo-2",
    "whole-word",
    "x",
];

/// Raster images: the app logo used inside the UI (the .ico is embedded by build.rs).
const IMAGES: &[(&str, &[u8])] = &[(
    "images/inkveil.png",
    include_bytes!("../assets/images/inkveil.png"),
)];

/// Serves [`ICONS`] and [`IMAGES`] to GPUI.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .chain(IMAGES)
            .find(|(p, _)| *p == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .chain(IMAGES)
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}

/// Names of icons used in the UI, so a typo is a compile error rather than a missing icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    AlignCenter,
    AlignLeft,
    AlignRight,
    ArrowDown,
    ArrowLeftRight,
    ArrowUp,
    Bold,
    CaseSensitive,
    Check,
    ChevronDown,
    ChevronRight,
    Code,
    CodeBlock,
    Columns,
    Copy,
    Divider,
    DollarSign,
    Ellipsis,
    Eye,
    EyeOff,
    FileText,
    Folder,
    FolderPlus,
    FontSize,
    Grip,
    Grok,
    Highlighter,
    History,
    Image,
    IndentDecrease,
    IndentIncrease,
    Italic,
    Link,
    List,
    ListChecks,
    ListOrdered,
    Lock,
    MessageCircle,
    NotebookPen,
    Pin,
    Plus,
    Quote,
    Redo,
    RemoveFormatting,
    Replace,
    ReplaceAll,
    Restore,
    Rows,
    Search,
    Settings,
    ShieldCheck,
    Sparkles,
    Stop,
    Strikethrough,
    Table,
    TextColor,
    Trash,
    Type,
    Underline,
    Undo,
    WholeWord,
    X,
}

impl Icon {
    pub fn path(self) -> &'static str {
        match self {
            Self::AlignCenter => "icons/align-center.svg",
            Self::AlignLeft => "icons/align-left.svg",
            Self::AlignRight => "icons/align-right.svg",
            Self::ArrowDown => "icons/arrow-down.svg",
            Self::ArrowLeftRight => "icons/arrow-left-right.svg",
            Self::ArrowUp => "icons/arrow-up.svg",
            Self::Bold => "icons/bold.svg",
            Self::CaseSensitive => "icons/case-sensitive.svg",
            Self::Check => "icons/check.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::Code => "icons/code.svg",
            Self::CodeBlock => "icons/square-code.svg",
            Self::Columns => "icons/columns-2.svg",
            Self::Copy => "icons/copy.svg",
            Self::Divider => "icons/minus.svg",
            Self::DollarSign => "icons/dollar-sign.svg",
            Self::Ellipsis => "icons/ellipsis.svg",
            Self::Eye => "icons/eye.svg",
            Self::EyeOff => "icons/eye-off.svg",
            Self::FileText => "icons/file-text.svg",
            Self::Folder => "icons/folder.svg",
            Self::FolderPlus => "icons/folder-plus.svg",
            Self::FontSize => "icons/a-large-small.svg",
            Self::Grip => "icons/grip-horizontal.svg",
            Self::Grok => "icons/grok.svg",
            Self::Highlighter => "icons/highlighter.svg",
            Self::History => "icons/history.svg",
            Self::Image => "icons/image.svg",
            Self::IndentDecrease => "icons/list-indent-decrease.svg",
            Self::IndentIncrease => "icons/list-indent-increase.svg",
            Self::Italic => "icons/italic.svg",
            Self::Link => "icons/link.svg",
            Self::List => "icons/list.svg",
            Self::ListChecks => "icons/list-checks.svg",
            Self::ListOrdered => "icons/list-ordered.svg",
            Self::Lock => "icons/lock.svg",
            Self::MessageCircle => "icons/message-circle.svg",
            Self::NotebookPen => "icons/notebook-pen.svg",
            Self::Pin => "icons/pin.svg",
            Self::Plus => "icons/plus.svg",
            Self::Quote => "icons/quote.svg",
            Self::Redo => "icons/redo-2.svg",
            Self::RemoveFormatting => "icons/remove-formatting.svg",
            Self::Replace => "icons/replace.svg",
            Self::ReplaceAll => "icons/replace-all.svg",
            Self::Restore => "icons/rotate-ccw.svg",
            Self::Rows => "icons/rows-2.svg",
            Self::Search => "icons/search.svg",
            Self::Settings => "icons/settings.svg",
            Self::ShieldCheck => "icons/shield-check.svg",
            Self::Sparkles => "icons/sparkles.svg",
            Self::Stop => "icons/square.svg",
            Self::Strikethrough => "icons/strikethrough.svg",
            Self::Table => "icons/table.svg",
            Self::TextColor => "icons/baseline.svg",
            Self::Trash => "icons/trash-2.svg",
            Self::Type => "icons/type.svg",
            Self::Underline => "icons/underline.svg",
            Self::Undo => "icons/undo-2.svg",
            Self::WholeWord => "icons/whole-word.svg",
            Self::X => "icons/x.svg",
        }
    }
}
