//! Multiple carets, like VS Code: typing, deleting and moving at every caret at once.
//!
//! The primary selection is [`Editor::selection`]; the others live in `Editor::extra`. Edits that
//! stay inside one text (typing, Backspace and Delete within a line, soft line breaks) apply at
//! every caret as one undo step. Edits that would join or split blocks, and every other command,
//! act on the primary selection only and drop the extra carets.

use crate::{
    Block, Editor, FindOptions, Pos, RichText, Selection, history::Grouping, search::find_in_str,
    text_ops,
};

impl Editor {
    /// All selections, the primary one first.
    pub fn selections(&self) -> Vec<Selection> {
        std::iter::once(self.selection)
            .chain(self.extra.iter().copied())
            .collect()
    }

    /// Whether there is more than one caret.
    pub fn has_multiple_carets(&self) -> bool {
        !self.extra.is_empty()
    }

    /// Replaces all selections; the first becomes the primary one. Overlapping selections (and
    /// carets at the same place) are merged; selections that only touch stay separate.
    pub fn set_selections(&mut self, selections: Vec<Selection>) {
        let mut selections: Vec<Selection> = selections
            .into_iter()
            .map(|s| Selection {
                anchor: self.doc.clamp(s.anchor),
                head: self.doc.clamp(s.head),
            })
            .collect();
        let Some(primary) = selections.first().copied() else {
            return;
        };
        selections.sort_by_key(|s| s.ordered().0);
        let mut merged: Vec<Selection> = Vec::with_capacity(selections.len());
        for s in selections {
            match merged.last_mut() {
                Some(last) if s.ordered().0 < last.ordered().1 || s.ordered() == last.ordered() => {
                    let (start, end) = (last.ordered().0, last.ordered().1.max(s.ordered().1));
                    *last = if start == end {
                        Selection::caret(start)
                    } else {
                        Selection {
                            anchor: start,
                            head: end,
                        }
                    };
                }
                _ => merged.push(s),
            }
        }
        let index = merged
            .iter()
            .position(|s| *s == primary)
            .or_else(|| {
                merged.iter().position(|s| {
                    let (a, b) = s.ordered();
                    a <= primary.head && primary.head <= b
                })
            })
            .unwrap_or(0);
        self.selection = merged.remove(index);
        self.extra = merged;
        self.stored_marks = None;
        self.history.break_group();
    }

    /// Adds a caret at `pos`, or removes the caret that is already there (Ctrl+click or
    /// Alt+click).
    pub fn toggle_caret(&mut self, pos: Pos) {
        let pos = self.doc.clamp(pos);
        let mut all = self.selections();
        if let Some(i) = all.iter().position(|s| s.is_collapsed() && s.head == pos) {
            if all.len() > 1 {
                all.remove(i);
                self.set_selections(all);
            }
            return;
        }
        all.insert(0, Selection::caret(pos));
        self.set_selections(all);
    }

    /// Adds a caret at `pos` and makes it the primary one (Ctrl+Alt+Up/Down).
    pub fn add_caret(&mut self, pos: Pos) {
        let mut all = self.selections();
        all.insert(0, Selection::caret(self.doc.clamp(pos)));
        self.set_selections(all);
    }

    /// Column (box) selection, like Shift+Alt+click or Shift+Alt+drag in VS Code: on every line
    /// from `from` to `to`, selects between the column of `from` and the column of `to`. When
    /// both columns are the same, every line gets a caret there. Shorter lines get their part at
    /// the line end. Lines are text blocks; tables and dividers are skipped.
    pub fn column_select(&mut self, from: Pos, to: Pos) {
        let (from, to) = (self.doc.clamp(from), self.doc.clamp(to));
        if from.loc.cell.is_some() || to.loc.cell.is_some() {
            self.set_selection(Selection {
                anchor: from,
                head: to,
            });
            return;
        }
        let column = |p: Pos| {
            self.doc
                .text(p.loc)
                .map_or(0, |t| t.as_str()[..p.offset].chars().count())
        };
        let (from_col, to_col) = (column(from), column(to));
        let (first, last) = (
            from.loc.block.min(to.loc.block),
            from.loc.block.max(to.loc.block),
        );
        let mut selections: Vec<Selection> = (first..=last)
            .filter_map(|i| {
                let text = self.doc.block(i)?.as_text()?.text.as_str();
                let at = |col: usize| text.char_indices().nth(col).map_or(text.len(), |(o, _)| o);
                Some(Selection {
                    anchor: Pos::new(i, at(from_col)),
                    head: Pos::new(i, at(to_col)),
                })
            })
            .collect();
        // The line under the pointer holds the primary selection.
        if let Some(i) = selections
            .iter()
            .position(|s| s.head.loc.block == to.loc.block)
        {
            let primary = selections.remove(i);
            selections.insert(0, primary);
        }
        if !selections.is_empty() {
            self.set_selections(selections);
        }
    }

    /// Puts a caret at the end of every line the selection touches (VS Code's Shift+Alt+I).
    /// Lines are text blocks; tables and dividers are skipped.
    pub fn carets_at_line_ends(&mut self) {
        let (start, end) = self.selection.ordered();
        if start.loc.block == end.loc.block {
            return;
        }
        let carets: Vec<Selection> = (start.loc.block..=end.loc.block)
            .rev()
            .filter_map(|i| {
                let text = self.doc.block(i)?.as_text()?;
                Some(Selection::caret(Pos::new(i, text.text.len())))
            })
            .collect();
        if !carets.is_empty() {
            self.set_selections(carets);
        }
    }

    /// Back to one caret: the primary selection.
    pub fn collapse_to_primary(&mut self) {
        self.extra.clear();
    }

    /// Selects the word at the caret, or adds the next occurrence of the selected text as a new
    /// selection (VS Code's Ctrl+D). Like VS Code, a selected whole word only matches whole
    /// words, so `asd` does not pick the inside of `asdasd`. Returns false when there is nothing
    /// more to add.
    pub fn select_next_occurrence(&mut self) -> bool {
        let primary = self.selection;
        if primary.is_collapsed() {
            self.select_word_at(primary.head);
            return !self.selection.is_collapsed();
        }
        if !primary.is_within_one_text() {
            return false;
        }
        let needle = self.selected_plain_text();
        let options = FindOptions {
            match_case: true,
            whole_word: self.is_whole_word(primary),
        };
        let taken = self.selections();
        // A match overlapping any existing selection is already taken.
        let is_taken = |loc, range: &std::ops::Range<usize>| {
            taken.iter().any(|s| {
                let (a, b) = s.ordered();
                a.loc == loc && a.offset < range.end && range.start < b.offset
            })
        };
        let (_, end) = primary.ordered();
        let texts: Vec<_> = self.doc.texts().map(|(loc, t)| (loc, t.clone())).collect();
        // Search forward from the primary selection, then wrap around.
        let after = texts.iter().flat_map(|(loc, text)| {
            find_in_str(text.as_str(), &needle, options)
                .into_iter()
                .map(move |r| (*loc, r))
        });
        let candidates: Vec<_> = after.collect();
        let next = candidates
            .iter()
            .find(|(loc, r)| {
                Pos {
                    loc: *loc,
                    offset: r.start,
                } >= end
                    && !is_taken(*loc, r)
            })
            .or_else(|| candidates.iter().find(|(loc, r)| !is_taken(*loc, r)))
            .cloned();
        let Some((loc, range)) = next else {
            return false;
        };
        let mut all = self.selections();
        all.insert(
            0,
            Selection {
                anchor: Pos {
                    loc,
                    offset: range.start,
                },
                head: Pos {
                    loc,
                    offset: range.end,
                },
            },
        );
        self.set_selections(all);
        true
    }

    /// Whether `selection` (inside one text) covers exactly one whole word.
    fn is_whole_word(&self, selection: Selection) -> bool {
        let (start, end) = selection.ordered();
        let Some(text) = self.doc.text(start.loc).map(RichText::as_str) else {
            return false;
        };
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let inside = text.get(start.offset..end.offset).unwrap_or("");
        !inside.is_empty()
            && inside.chars().all(word)
            && !text[..start.offset].chars().next_back().is_some_and(word)
            && !text[end.offset..].chars().next().is_some_and(word)
    }

    /// Types `text` at every caret, replacing each selection. Returns false, changing nothing,
    /// when that is not possible (text with line breaks between blocks, or a selection that
    /// spans several texts).
    pub(crate) fn multi_insert(&mut self, text: &str) -> bool {
        let all = self.selections();
        if text.contains('\r') || all.iter().any(|s| !s.is_within_one_text()) {
            return false;
        }
        let grouping = if all.iter().all(Selection::is_collapsed) && !text.contains('\n') {
            Grouping::Typing
        } else {
            Grouping::Separate
        };
        let edits: Vec<(Pos, Pos)> = all.iter().map(Selection::ordered).collect();
        self.apply_at_carets(grouping, &edits, |t, start, end| {
            let marks = t.marks_for_insert(start);
            t.delete(start..end);
            t.insert(start, text, marks);
            start + text.len()
        });
        true
    }

    /// Backspace or Delete at every caret. Carets at the edge of their text do nothing, since
    /// joining blocks at several places at once is rarely what anyone wants.
    pub(crate) fn multi_delete(&mut self, forward: bool, unit: crate::Unit) {
        let all = self.selections();
        if all.iter().any(|s| !s.is_within_one_text()) {
            self.extra.clear();
            if forward {
                self.delete_forward(unit);
            } else {
                self.delete_backward(unit);
            }
            return;
        }
        let edits: Vec<(Pos, Pos)> = all
            .iter()
            .map(|s| {
                if !s.is_collapsed() {
                    return s.ordered();
                }
                let caret = s.head;
                let text = self.doc.text(caret.loc).map_or("", RichText::as_str);
                let other = match (forward, unit) {
                    (false, _) if caret.offset == 0 => caret.offset,
                    (true, _) if caret.offset >= text.len() => caret.offset,
                    (false, crate::Unit::Grapheme) => text_ops::prev_grapheme(text, caret.offset),
                    (false, crate::Unit::Word) => text_ops::prev_word(text, caret.offset),
                    (true, crate::Unit::Grapheme) => text_ops::next_grapheme(text, caret.offset),
                    (true, crate::Unit::Word) => text_ops::next_word(text, caret.offset),
                };
                let (a, b) = (caret.offset.min(other), caret.offset.max(other));
                (Pos { offset: a, ..caret }, Pos { offset: b, ..caret })
            })
            .collect();
        let grouping = if all.iter().all(Selection::is_collapsed) {
            Grouping::Typing
        } else {
            Grouping::Separate
        };
        self.apply_at_carets(grouping, &edits, |t, start, end| {
            t.delete(start..end);
            start
        });
    }

    /// Applies `edit` to each `(start, end)` range (each inside one text) as one undo step.
    /// `edit` changes the text and returns where the caret goes. The first range belongs to the
    /// primary selection.
    fn apply_at_carets(
        &mut self,
        grouping: Grouping,
        edits: &[(Pos, Pos)],
        edit: impl Fn(&mut RichText, usize, usize) -> usize,
    ) {
        let mut order: Vec<usize> = (0..edits.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(edits[i].0));
        self.change(grouping, |ed| {
            // Back to front, so earlier ranges stay valid; carets already placed after an edit in
            // the same text are shifted by the length it added or removed.
            let mut carets: Vec<Option<Pos>> = vec![None; edits.len()];
            for &i in &order {
                let (start, end) = edits[i];
                let Some(text) = ed.doc.text_mut(start.loc) else {
                    continue;
                };
                let before = text.len();
                let caret = edit(text, start.offset, end.offset);
                let delta = text.len().cast_signed() - before.cast_signed();
                for placed in carets.iter_mut().flatten() {
                    if placed.loc == start.loc && placed.offset >= end.offset {
                        placed.offset = placed.offset.saturating_add_signed(delta);
                    }
                }
                carets[i] = Some(Pos {
                    loc: start.loc,
                    offset: caret,
                });
            }
            let mut carets = carets.into_iter().flatten();
            if let Some(primary) = carets.next() {
                ed.selection = Selection::caret(primary);
            }
            let mut extra: Vec<Pos> = carets.filter(|p| *p != ed.selection.head).collect();
            extra.sort();
            extra.dedup();
            ed.extra = extra.into_iter().map(Selection::caret).collect();
            ed.keep_extra = true;
        });
    }

    /// Duplicates the selected lines (whole blocks) below themselves and moves the selection to
    /// the copy (Ctrl+D).
    pub fn duplicate_lines(&mut self) {
        let (start, end) = self.selection.ordered();
        let (first, last) = (start.loc.block, end.loc.block);
        let copies: Vec<Block> = (first..=last)
            .filter_map(|i| self.doc.block(i).cloned())
            .collect();
        if copies.is_empty() {
            return;
        }
        let count = copies.len();
        self.change(Grouping::Separate, |ed| {
            let at = last + 1;
            ed.doc
                .blocks
                .splice(at..at, copies.into_iter().map(std::sync::Arc::new));
            let shift = |p: Pos| Pos {
                loc: crate::Loc {
                    block: p.loc.block + count,
                    ..p.loc
                },
                ..p
            };
            ed.selection = Selection {
                anchor: shift(ed.selection.anchor),
                head: shift(ed.selection.head),
            };
        });
    }
}
