use crate::CellRef;

/// Where a piece of text lives: a text block, or one cell of a table block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Loc {
    /// Block index.
    pub block: usize,
    /// The cell, when the block is a table.
    pub cell: Option<CellRef>,
}

impl Loc {
    /// A text block.
    pub fn block(block: usize) -> Self {
        Self { block, cell: None }
    }

    /// A table cell.
    pub fn cell(block: usize, row: u16, col: u16) -> Self {
        Self {
            block,
            cell: Some(CellRef { row, col }),
        }
    }
}

/// A caret position: a byte offset into the text at a [`Loc`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pos {
    /// Where the text is.
    pub loc: Loc,
    /// Byte offset into it, on a char boundary.
    pub offset: usize,
}

impl Pos {
    /// A position in a text block.
    pub fn new(block: usize, offset: usize) -> Self {
        Self {
            loc: Loc::block(block),
            offset,
        }
    }

    /// A position in a table cell.
    pub fn in_cell(block: usize, row: u16, col: u16, offset: usize) -> Self {
        Self {
            loc: Loc::cell(block, row, col),
            offset,
        }
    }
}

/// A selection: the fixed `anchor` and the moving `head` (where the caret is drawn).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Selection {
    /// Where the selection started.
    pub anchor: Pos,
    /// Where the caret is.
    pub head: Pos,
}

impl Selection {
    /// A caret with nothing selected.
    pub fn caret(pos: Pos) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
    }

    /// Whether nothing is selected.
    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.head
    }

    /// Start and end in document order.
    pub fn ordered(&self) -> (Pos, Pos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Whether both ends are in the same text (same block, same cell).
    pub fn is_within_one_text(&self) -> bool {
        self.anchor.loc == self.head.loc
    }
}
