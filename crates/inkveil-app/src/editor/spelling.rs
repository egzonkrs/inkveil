//! Which words of a document are misspelled, cached per block.

use std::{ops::Range, sync::Arc};

use inkveil_doc::{Block, CellRef, Document, Loc, RichText, TextKind, diff_blocks};

use crate::spell;

/// Misspelled ranges of one block: `(cell, byte range)`, `cell` is `None` for text blocks.
type BlockErrors = Vec<(Option<CellRef>, Range<usize>)>;

#[derive(Default)]
pub struct Spelling {
    generation: u64,
    source: Vec<Arc<Block>>,
    per_block: Vec<BlockErrors>,
}

impl Spelling {
    /// Re-checks blocks that changed since the last call. Returns true if anything changed.
    pub fn update(&mut self, doc: &Document) -> bool {
        let generation = spell::generation();
        if generation == 0 {
            return false;
        }
        let (start, old_end, new_end) = if generation == self.generation {
            let diff = diff_blocks(&self.source, doc.blocks());
            if diff.is_empty() {
                return false;
            }
            (diff.start, diff.old_end, diff.new_end)
        } else {
            (0, self.per_block.len(), doc.len())
        };
        let fresh: Vec<BlockErrors> = doc.blocks()[start..new_end]
            .iter()
            .map(|b| check_block(b))
            .collect();
        self.per_block.splice(start..old_end, fresh);
        self.source = doc.blocks().to_vec();
        self.generation = generation;
        true
    }

    /// All misspelled ranges, except the word the caret is at the end of (still being typed).
    pub fn ranges(&self, caret: inkveil_doc::Pos) -> Vec<(Loc, Range<usize>)> {
        self.per_block
            .iter()
            .enumerate()
            .flat_map(|(block, errors)| {
                errors
                    .iter()
                    .map(move |(cell, range)| (Loc { block, cell: *cell }, range.clone()))
            })
            .filter(|(loc, range)| !(*loc == caret.loc && range.end == caret.offset))
            .collect()
    }
}

fn check_block(block: &Block) -> BlockErrors {
    match block {
        Block::Text(t) if t.kind != TextKind::Code => {
            check_text(&t.text).into_iter().map(|r| (None, r)).collect()
        }
        Block::Table(table) => table
            .rows()
            .iter()
            .enumerate()
            .flat_map(|(r, row)| {
                row.iter().enumerate().flat_map(move |(c, cell)| {
                    let cell_ref = CellRef {
                        row: u16::try_from(r).unwrap_or(0),
                        col: u16::try_from(c).unwrap_or(0),
                    };
                    check_text(cell)
                        .into_iter()
                        .map(move |range| (Some(cell_ref), range))
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Misspelled words outside inline code and links.
fn check_text(text: &RichText) -> Vec<Range<usize>> {
    spell::misspelled(text.as_str())
        .into_iter()
        .filter(|range| {
            !text.run_ranges().any(|(r, m)| {
                (m.code || m.link.is_some()) && r.start < range.end && range.start < r.end
            })
        })
        .collect()
}
