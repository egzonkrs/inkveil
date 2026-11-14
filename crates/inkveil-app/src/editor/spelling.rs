//! Which words of a document are misspelled, cached per block.

use std::{ops::Range, sync::Arc};

use inkveil_doc::{Block, CellRef, Document, Loc, RichText, TextKind, diff_blocks};

use crate::spell;

/// Misspelled ranges of one block: `(cell, byte range)`, `cell` is `None` for text blocks.
type BlockErrors = Vec<(Option<CellRef>, Range<usize>)>;

/// How long one call may spend checking a whole note, and how many blocks at most. A long
/// note is checked over several frames instead of holding up the one it opens in.
const SLICE: std::time::Duration = std::time::Duration::from_millis(4);
const SLICE_BLOCKS: usize = 400;

#[derive(Default)]
pub struct Spelling {
    generation: u64,
    source: Vec<Arc<Block>>,
    per_block: Vec<BlockErrors>,
    /// Blocks from here on have not been checked yet since the whole note needed checking.
    unchecked_from: Option<usize>,
}

impl Spelling {
    /// Re-checks blocks that changed since the last call. Returns true if anything changed.
    ///
    /// A whole note (just opened, or after the dictionary changed) is checked a slice at a
    /// time: call again while [`Self::pending`] says there is more.
    pub fn update(&mut self, doc: &Document) -> bool {
        let generation = spell::generation();
        if generation == 0 {
            return false;
        }
        let mut changed = false;
        let diff = diff_blocks(&self.source, doc.blocks());
        let whole = generation != self.generation
            || self.per_block.len() != self.source.len()
            || diff.new_end - diff.start > SLICE_BLOCKS;
        if whole {
            self.per_block = vec![BlockErrors::new(); doc.len()];
            self.source = doc.blocks().to_vec();
            self.generation = generation;
            self.unchecked_from = Some(0);
            changed = true;
        } else if !diff.is_empty() {
            let fresh: Vec<BlockErrors> = doc.blocks()[diff.start..diff.new_end]
                .iter()
                .map(|b| check_block(b))
                .collect();
            self.per_block.splice(diff.start..diff.old_end, fresh);
            self.source = doc.blocks().to_vec();
            // Where the unchecked rest starts now that blocks were added or removed.
            self.unchecked_from = self.unchecked_from.map(|from| {
                if from >= diff.old_end {
                    from - diff.old_end + diff.new_end
                } else {
                    from.min(diff.start)
                }
            });
            changed = true;
        }
        changed | self.check_some()
    }

    /// Checks the next slice of a note that is being checked as a whole.
    fn check_some(&mut self) -> bool {
        let Some(mut next) = self.unchecked_from else {
            return false;
        };
        let started = std::time::Instant::now();
        let end = self.source.len().min(next + SLICE_BLOCKS);
        while next < end {
            self.per_block[next] = check_block(&self.source[next]);
            next += 1;
            if started.elapsed() >= SLICE {
                break;
            }
        }
        self.unchecked_from = (next < self.source.len()).then_some(next);
        true
    }

    /// Whether part of the note is still waiting to be checked.
    pub fn pending(&self) -> bool {
        self.unchecked_from.is_some()
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
