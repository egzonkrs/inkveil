//! Undo and redo.
//!
//! Each entry records the block range a command replaced: the blocks that were there before and
//! how many blocks replaced them. Blocks are shared, so an entry holds only changed blocks.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{Block, Selection, document::BlockDiff};

/// Typing within this long of the previous keystroke joins the same undo step.
const GROUP_INTERVAL: Duration = Duration::from_millis(1000);
/// Undo steps kept per note.
const MAX_ENTRIES: usize = 500;

/// How a change may be grouped with the previous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Grouping {
    /// Always a separate undo step.
    Separate,
    /// Joins a preceding typing step in the same place.
    Typing,
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    /// Index where the change starts.
    start: usize,
    /// Blocks that were replaced.
    old: Vec<Arc<Block>>,
    /// How many blocks replaced them.
    new_len: usize,
    selection_before: Selection,
    selection_after: Selection,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    last_typing: Option<Instant>,
}

impl History {
    /// Records a change, given the block list before it and the diff.
    pub(crate) fn record(
        &mut self,
        before: &[Arc<Block>],
        diff: BlockDiff,
        selection_before: Selection,
        selection_after: Selection,
        grouping: Grouping,
    ) {
        self.redo.clear();
        let now = Instant::now();
        let joins = grouping == Grouping::Typing
            && self
                .last_typing
                .is_some_and(|t| now.duration_since(t) < GROUP_INTERVAL)
            && self.undo.last().is_some_and(|prev| {
                prev.selection_after == selection_before
                    && prev.start == diff.start
                    && prev.start + prev.new_len == diff.old_end
            });
        self.last_typing = (grouping == Grouping::Typing).then_some(now);

        if joins {
            let prev = self.undo.last_mut().expect("checked above");
            prev.new_len = diff.new_end - diff.start;
            prev.selection_after = selection_after;
            return;
        }
        self.undo.push(Entry {
            start: diff.start,
            old: before[diff.start..diff.old_end].to_vec(),
            new_len: diff.new_end - diff.start,
            selection_before,
            selection_after,
        });
        if self.undo.len() > MAX_ENTRIES {
            self.undo.remove(0);
        }
    }

    /// Stops the current typing group, so the next keystroke starts a new undo step.
    pub(crate) fn break_group(&mut self) {
        self.last_typing = None;
    }

    pub(crate) fn undo(&mut self, blocks: &mut Vec<Arc<Block>>) -> Option<Selection> {
        let entry = self.undo.pop()?;
        let inverse = apply(blocks, &entry);
        self.redo.push(inverse);
        self.last_typing = None;
        Some(entry.selection_before)
    }

    pub(crate) fn redo(&mut self, blocks: &mut Vec<Arc<Block>>) -> Option<Selection> {
        let entry = self.redo.pop()?;
        let inverse = apply(blocks, &entry);
        self.undo.push(inverse);
        self.last_typing = None;
        Some(entry.selection_before)
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

/// Applies `entry` and returns the entry that reverses it.
fn apply(blocks: &mut Vec<Arc<Block>>, entry: &Entry) -> Entry {
    let range = entry.start..entry.start + entry.new_len;
    let removed: Vec<Arc<Block>> = blocks.splice(range, entry.old.iter().cloned()).collect();
    Entry {
        start: entry.start,
        new_len: entry.old.len(),
        old: removed,
        selection_before: entry.selection_after,
        selection_after: entry.selection_before,
    }
}
