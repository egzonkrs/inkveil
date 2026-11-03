use serde::{Deserialize, Serialize};

use crate::RichText;

/// The deepest list or paragraph indent level.
pub(crate) const MAX_INDENT: u8 = 8;

/// What kind of text block this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TextKind {
    /// A normal paragraph.
    Paragraph,
    /// A heading, level 1 to 3.
    Heading(u8),
    /// A bulleted list item.
    Bullet,
    /// A numbered list item. Numbers come from consecutive items at the same indent.
    Numbered,
    /// A checklist item.
    Task {
        /// Whether the box is ticked.
        checked: bool,
    },
    /// A block quote.
    Quote,
    /// A code block. Enter inserts a line break instead of a new block.
    Code,
}

impl TextKind {
    /// Whether this is a list item (bullet, numbered or task).
    pub fn is_list(self) -> bool {
        matches!(self, Self::Bullet | Self::Numbered | Self::Task { .. })
    }

    /// Kind of block that Enter at the end of this one creates.
    pub fn continuation(self) -> Self {
        match self {
            Self::Heading(_) => Self::Paragraph,
            Self::Task { .. } => Self::Task { checked: false },
            other => other,
        }
    }

    /// Whether `self` and `other` are the same kind, ignoring a task's checked state.
    pub fn same_kind(self, other: Self) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
            && match (self, other) {
                (Self::Heading(a), Self::Heading(b)) => a == b,
                _ => true,
            }
    }
}

/// Horizontal alignment of a block's lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Align {
    /// Aligned to the left edge.
    #[default]
    Left,
    /// Centered.
    Center,
    /// Aligned to the right edge.
    Right,
}

/// Text size of a block, relative to the editor's base size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FontSize {
    /// Smaller than body text.
    Small,
    /// Body text.
    #[default]
    Normal,
    /// Larger than body text.
    Large,
    /// Much larger than body text.
    Huge,
}

impl FontSize {
    /// Scale factor relative to body text.
    pub fn scale(self) -> f32 {
        match self {
            Self::Small => 0.85,
            Self::Normal => 1.0,
            Self::Large => 1.25,
            Self::Huge => 1.6,
        }
    }
}

/// A block of rich text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextBlock {
    /// Paragraph, heading, list item, ...
    pub kind: TextKind,
    /// Line alignment.
    pub align: Align,
    /// Indent level, `0..=MAX_INDENT`.
    pub indent: u8,
    /// Text size.
    pub size: FontSize,
    /// The content.
    pub text: RichText,
}

impl TextBlock {
    /// An empty paragraph.
    pub fn empty() -> Self {
        Self::paragraph(RichText::new())
    }

    /// A paragraph with the given text.
    pub fn paragraph(text: RichText) -> Self {
        Self {
            kind: TextKind::Paragraph,
            align: Align::Left,
            indent: 0,
            size: FontSize::Normal,
            text,
        }
    }

    /// A block of the same kind and layout as `self`, holding `text`.
    pub fn with_text(&self, text: RichText) -> Self {
        Self {
            text,
            ..self.clone()
        }
    }
}

/// Address of a table cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CellRef {
    /// Row index.
    pub row: u16,
    /// Column index.
    pub col: u16,
}

/// A table of rich-text cells. Always rectangular, at least 1×1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Table {
    rows: Vec<Vec<RichText>>,
}

impl Table {
    /// The largest number of rows or columns a table may have.
    pub const MAX_SIZE: u16 = 64;

    /// An empty table with the given size, clamped to `1..=MAX_SIZE`.
    pub fn new(rows: u16, cols: u16) -> Self {
        let rows = rows.clamp(1, Self::MAX_SIZE);
        let cols = cols.clamp(1, Self::MAX_SIZE);
        Self {
            rows: vec![vec![RichText::new(); usize::from(cols)]; usize::from(rows)],
        }
    }

    /// Number of rows.
    pub fn row_count(&self) -> u16 {
        u16::try_from(self.rows.len()).unwrap_or(Self::MAX_SIZE)
    }

    /// Number of columns.
    pub fn col_count(&self) -> u16 {
        u16::try_from(self.rows.first().map_or(0, Vec::len)).unwrap_or(Self::MAX_SIZE)
    }

    /// The rows of cells.
    pub fn rows(&self) -> &[Vec<RichText>] {
        &self.rows
    }

    /// One cell.
    pub fn cell(&self, at: CellRef) -> Option<&RichText> {
        self.rows
            .get(usize::from(at.row))
            .and_then(|r| r.get(usize::from(at.col)))
    }

    pub(crate) fn cell_mut(&mut self, at: CellRef) -> Option<&mut RichText> {
        self.rows
            .get_mut(usize::from(at.row))
            .and_then(|r| r.get_mut(usize::from(at.col)))
    }

    /// Inserts an empty row at index `row`.
    pub(crate) fn insert_row(&mut self, row: u16) -> bool {
        if self.row_count() >= Self::MAX_SIZE {
            return false;
        }
        let at = usize::from(row).min(self.rows.len());
        let cols = usize::from(self.col_count());
        self.rows.insert(at, vec![RichText::new(); cols]);
        true
    }

    /// Inserts an empty column at index `col`.
    pub(crate) fn insert_col(&mut self, col: u16) -> bool {
        if self.col_count() >= Self::MAX_SIZE {
            return false;
        }
        for row in &mut self.rows {
            let at = usize::from(col).min(row.len());
            row.insert(at, RichText::new());
        }
        true
    }

    /// Removes a row. Refuses to remove the last one.
    pub(crate) fn remove_row(&mut self, row: u16) -> bool {
        if self.rows.len() <= 1 || usize::from(row) >= self.rows.len() {
            return false;
        }
        self.rows.remove(usize::from(row));
        true
    }

    /// Removes a column. Refuses to remove the last one.
    pub(crate) fn remove_col(&mut self, col: u16) -> bool {
        if self.col_count() <= 1 || col >= self.col_count() {
            return false;
        }
        for row in &mut self.rows {
            row.remove(usize::from(col));
        }
        true
    }

    pub(crate) fn is_valid(&self) -> bool {
        let cols = self.rows.first().map_or(0, Vec::len);
        cols > 0
            && self.rows.len() <= usize::from(Self::MAX_SIZE)
            && cols <= usize::from(Self::MAX_SIZE)
            && self
                .rows
                .iter()
                .all(|r| r.len() == cols && r.iter().all(RichText::is_valid))
    }
}

/// A picture stored in the vault as an encrypted attachment; the note keeps only its id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageBlock {
    /// The vault attachment holding the picture.
    pub blob: [u8; 16],
    /// Size of the picture in pixels, so the layout can reserve space before it loads.
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Display width as a percentage of the text column; never wider than the picture itself.
    pub width_percent: u8,
    pub align: Align,
}

impl ImageBlock {
    /// The smallest display width, in percent.
    pub const MIN_PERCENT: u8 = 10;
    /// The largest accepted picture side, in pixels.
    pub const MAX_SIDE: u32 = 20_000;

    pub(crate) fn is_valid(&self) -> bool {
        (Self::MIN_PERCENT..=100).contains(&self.width_percent)
            && (1..=Self::MAX_SIDE).contains(&self.pixel_width)
            && (1..=Self::MAX_SIDE).contains(&self.pixel_height)
    }
}

/// One block of a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Block {
    /// Paragraph, heading, list item, quote or code.
    Text(TextBlock),
    /// A horizontal rule.
    Divider,
    /// A table.
    Table(Table),
    /// A picture. (Added after the first stored format; new variants go last so older notes
    /// still decode.)
    Image(ImageBlock),
}

impl Block {
    /// An empty paragraph.
    pub fn empty_paragraph() -> Self {
        Self::Text(TextBlock::empty())
    }

    /// The text block, if this is one.
    pub fn as_text(&self) -> Option<&TextBlock> {
        match self {
            Self::Text(t) => Some(t),
            _ => None,
        }
    }

    /// The image, if this is one.
    pub fn as_image(&self) -> Option<&ImageBlock> {
        match self {
            Self::Image(i) => Some(i),
            _ => None,
        }
    }

    /// The table, if this is one.
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Self::Table(t) => Some(t),
            _ => None,
        }
    }

    /// Whether this is a text block.
    pub fn is_text(&self) -> bool {
        matches!(self, Self::Text(_))
    }
}
