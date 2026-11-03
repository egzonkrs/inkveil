use std::sync::Arc;

use crate::{Block, Loc, Pos, RichText, block::MAX_INDENT};

/// A rich-text document: a list of blocks.
///
/// Invariants, restored by [`Document::normalize`] after every edit:
/// - there is at least one block, and the first and last blocks are text blocks;
/// - every divider or table is directly followed by a text block, so the caret can always be
///   placed after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub(crate) blocks: Vec<Arc<Block>>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// A document with one empty paragraph.
    pub fn new() -> Self {
        Self {
            blocks: vec![Arc::new(Block::empty_paragraph())],
        }
    }

    /// A document from blocks, normalized.
    pub fn from_blocks(blocks: impl IntoIterator<Item = Block>) -> Self {
        let mut doc = Self {
            blocks: blocks.into_iter().map(Arc::new).collect(),
        };
        doc.normalize();
        doc
    }

    /// A document of plain paragraphs, one per line of `text`.
    pub fn from_plain_text(text: &str) -> Self {
        Self::from_blocks(
            text.lines()
                .map(|line| Block::Text(crate::TextBlock::paragraph(RichText::plain(line)))),
        )
    }

    /// The blocks.
    pub fn blocks(&self) -> &[Arc<Block>] {
        &self.blocks
    }

    /// The block at `index`.
    pub fn block(&self, index: usize) -> Option<&Block> {
        self.blocks.get(index).map(AsRef::as_ref)
    }

    /// Number of blocks.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether the document is a single empty paragraph.
    pub fn is_empty(&self) -> bool {
        self.blocks.len() == 1 && self.blocks[0].as_text().is_some_and(|t| t.text.is_empty())
    }

    /// The text at `loc`, if `loc` points at a text block or an existing cell.
    pub fn text(&self, loc: Loc) -> Option<&RichText> {
        match (self.block(loc.block)?, loc.cell) {
            (Block::Text(t), None) => Some(&t.text),
            (Block::Table(table), Some(cell)) => table.cell(cell),
            _ => None,
        }
    }

    pub(crate) fn text_mut(&mut self, loc: Loc) -> Option<&mut RichText> {
        let block = Arc::make_mut(self.blocks.get_mut(loc.block)?);
        match (block, loc.cell) {
            (Block::Text(t), None) => Some(&mut t.text),
            (Block::Table(table), Some(cell)) => table.cell_mut(cell),
            _ => None,
        }
    }

    pub(crate) fn block_mut(&mut self, index: usize) -> Option<&mut Block> {
        self.blocks.get_mut(index).map(Arc::make_mut)
    }

    /// The first caret position.
    pub fn start(&self) -> Pos {
        Pos::new(0, 0)
    }

    /// The last caret position.
    pub fn end(&self) -> Pos {
        let last = self.blocks.len() - 1;
        Pos::new(last, self.text(Loc::block(last)).map_or(0, RichText::len))
    }

    /// The nearest valid caret position to `pos`.
    pub fn clamp(&self, pos: Pos) -> Pos {
        if pos.loc.block >= self.blocks.len() {
            return self.end();
        }
        if let Some(text) = self.text(pos.loc) {
            let mut offset = pos.offset.min(text.len());
            while !text.as_str().is_char_boundary(offset) {
                offset -= 1;
            }
            return Pos {
                loc: pos.loc,
                offset,
            };
        }
        match self.block(pos.loc.block) {
            Some(Block::Table(table)) => {
                let row = pos.loc.cell.map_or(0, |c| c.row).min(table.row_count() - 1);
                let col = pos.loc.cell.map_or(0, |c| c.col).min(table.col_count() - 1);
                self.clamp(Pos::in_cell(pos.loc.block, row, col, pos.offset))
            }
            // A divider: the caret goes to the start of the following text block.
            _ => Pos::new(pos.loc.block + 1, 0),
        }
    }

    /// All text, one line per block (table cells separated by tabs).
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for (i, block) in self.blocks.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            match block.as_ref() {
                Block::Text(t) => out.push_str(t.text.as_str()),
                Block::Divider => out.push_str("---"),
                Block::Image(_) => {}
                Block::Table(table) => {
                    for (r, row) in table.rows().iter().enumerate() {
                        if r > 0 {
                            out.push('\n');
                        }
                        let cells: Vec<&str> = row.iter().map(RichText::as_str).collect();
                        out.push_str(&cells.join("\t"));
                    }
                }
            }
        }
        out
    }

    /// Counts whitespace-delimited words across all text and table blocks without allocating.
    pub fn word_count(&self) -> usize {
        self.blocks
            .iter()
            .map(|block| match block.as_ref() {
                Block::Text(t) => t.text.as_str().split_whitespace().count(),
                Block::Divider => 1,
                Block::Image(_) => 0,
                Block::Table(table) => table
                    .rows()
                    .iter()
                    .flatten()
                    .map(|cell| cell.as_str().split_whitespace().count())
                    .sum(),
            })
            .sum()
    }

    /// Restores the invariants listed on [`Document`].
    pub(crate) fn normalize(&mut self) {
        if self.blocks.first().is_none_or(|b| !b.is_text()) {
            self.blocks.insert(0, Arc::new(Block::empty_paragraph()));
        }
        let mut i = 0;
        while i < self.blocks.len() {
            if !self.blocks[i].is_text()
                && self.blocks.get(i + 1).is_none_or(|next| !next.is_text())
            {
                self.blocks
                    .insert(i + 1, Arc::new(Block::empty_paragraph()));
            }
            if let Block::Text(t) = self.blocks[i].as_ref()
                && t.indent > MAX_INDENT
                && let Some(Block::Text(t)) = self.block_mut(i)
            {
                t.indent = MAX_INDENT;
            }
            i += 1;
        }
    }

    /// Checks the invariants. Used by tests.
    pub fn is_valid(&self) -> bool {
        let structure = self.blocks.first().is_some_and(|b| b.is_text())
            && self.blocks.last().is_some_and(|b| b.is_text())
            && self
                .blocks
                .windows(2)
                .all(|w| w[0].is_text() || w[1].is_text());
        let contents = self.blocks.iter().all(|b| match b.as_ref() {
            Block::Text(t) => t.text.is_valid() && t.indent <= MAX_INDENT,
            Block::Table(table) => table.is_valid(),
            Block::Divider => true,
            Block::Image(image) => image.is_valid(),
        });
        structure && contents
    }
}

/// The block range that changed between two versions of a block list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockDiff {
    /// First changed index (same in both lists).
    pub start: usize,
    /// End of the changed range in the old list.
    pub old_end: usize,
    /// End of the changed range in the new list.
    pub new_end: usize,
}

impl BlockDiff {
    /// Whether nothing changed.
    pub fn is_empty(&self) -> bool {
        self.start == self.old_end && self.start == self.new_end
    }
}

/// Finds the changed range between `old` and `new` by comparing block identity.
///
/// Blocks are copy-on-write, so an unchanged block is the very same allocation in both lists and
/// this needs only pointer comparisons.
pub fn diff_blocks(old: &[Arc<Block>], new: &[Arc<Block>]) -> BlockDiff {
    let prefix = old
        .iter()
        .zip(new)
        .take_while(|(a, b)| Arc::ptr_eq(a, b))
        .count();
    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| Arc::ptr_eq(a, b))
        .count();
    BlockDiff {
        start: prefix,
        old_end: old.len() - suffix,
        new_end: new.len() - suffix,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Table, TextBlock};

    #[test]
    fn normalize_adds_text_around_objects() {
        let doc = Document::from_blocks([Block::Divider, Block::Table(Table::new(2, 2))]);
        assert!(doc.is_valid());
        assert_eq!(doc.len(), 5);
        assert!(doc.blocks[0].is_text() && doc.blocks[2].is_text() && doc.blocks[4].is_text());
    }

    #[test]
    fn clamp_moves_off_dividers_and_into_cells() {
        let doc = Document::from_blocks([
            Block::Text(TextBlock::paragraph(RichText::plain("ab"))),
            Block::Divider,
            Block::Text(TextBlock::empty()),
            Block::Table(Table::new(1, 1)),
        ]);
        assert_eq!(doc.clamp(Pos::new(0, 99)), Pos::new(0, 2));
        assert_eq!(doc.clamp(Pos::new(1, 0)), Pos::new(2, 0));
        assert_eq!(doc.clamp(Pos::new(3, 0)), Pos::in_cell(3, 0, 0, 0));
        assert_eq!(doc.clamp(Pos::new(99, 0)), doc.end());
    }

    #[test]
    fn diff_finds_changed_range() {
        let a: Vec<Arc<Block>> = (0..5).map(|_| Arc::new(Block::empty_paragraph())).collect();
        let mut b = a.clone();
        b[2] = Arc::new(Block::Divider);
        b.insert(3, Arc::new(Block::Divider));
        assert_eq!(
            diff_blocks(&a, &b),
            BlockDiff {
                start: 2,
                old_end: 3,
                new_end: 4
            }
        );
        assert!(diff_blocks(&a, &a).is_empty());
    }
}
