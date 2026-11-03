//! The rich-text document model behind the Inkveil editor.
//!
//! A [`Document`] is a flat list of [`Block`]s: text blocks (paragraphs, headings, list items,
//! quotes, code), dividers and tables. Text is stored as [`RichText`]: a string plus runs of
//! inline [`Marks`] (bold, italic, color, links, ...).
//!
//! All editing goes through [`Editor`], which owns the document, the selection and the undo
//! history. Every command is a pure function of that state, so the whole editing engine is tested
//! without a window. The UI only lays out and paints blocks and turns input into commands.
//!
//! Blocks are reference counted. A command clones the block list (cheap: one pointer per block),
//! edits blocks copy-on-write, and the history keeps only the blocks that changed.

#![forbid(unsafe_code)]

mod block;
mod document;
mod editor;
mod format;
mod history;
mod marks;
mod multi;
mod position;
mod rich_text;
mod search;
mod text_ops;

pub use block::{Align, Block, CellRef, FontSize, ImageBlock, Table, TextBlock, TextKind};
pub use document::{BlockDiff, Document, diff_blocks};
pub use editor::{Editor, Fragment, MarkKind, Motion, Unit};
pub use format::DecodeError;
pub use marks::{Marks, Rgb};
pub use position::{Loc, Pos, Selection};
pub use rich_text::{RichText, Run};
pub use search::{FindOptions, Match, find_in_str};
