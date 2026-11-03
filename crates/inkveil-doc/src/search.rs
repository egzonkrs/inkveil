//! Finding text in a document, and replacing what was found.

use std::ops::Range;

use crate::{Block, Document, Editor, Loc, Marks, Pos, RichText, Selection, history::Grouping};

/// How to match the query.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FindOptions {
    /// Upper and lower case must match exactly.
    pub match_case: bool,
    /// Only matches that are whole words.
    pub whole_word: bool,
}

/// One match: a byte range of the text at `loc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub loc: Loc,
    pub range: Range<usize>,
}

/// Byte ranges of `query` in `text`, in order and not overlapping.
pub fn find_in_str(text: &str, query: &str, options: FindOptions) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if options.match_case {
        for (start, found) in text.match_indices(query) {
            out.push(start..start + found.len());
        }
    } else {
        // Search a lowercased copy and map positions back. Lowercasing can change byte lengths,
        // so remember where each lowercased byte came from.
        let mut lower = String::with_capacity(text.len());
        let mut origin = Vec::with_capacity(text.len());
        for (index, ch) in text.char_indices() {
            for lc in ch.to_lowercase() {
                lower.push(lc);
                origin.extend(std::iter::repeat_n(index, lc.len_utf8()));
            }
        }
        let query = query.to_lowercase();
        let back = |i: usize| origin.get(i).copied().unwrap_or(text.len());
        for (start, found) in lower.match_indices(&query) {
            let (from, to) = (back(start), back(start + found.len()));
            if from < to {
                out.push(from..to);
            }
        }
    }
    if options.whole_word {
        out.retain(|r| is_word_edge(text, r.start) && is_word_edge(text, r.end));
    }
    out
}

/// Whether a word can start or end at `offset`: the characters on both sides are not both
/// letters, digits or underscores.
fn is_word_edge(text: &str, offset: usize) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let before = text[..offset].chars().next_back().is_some_and(word);
    let after = text[offset..].chars().next().is_some_and(word);
    !(before && after)
}

impl Document {
    /// Every text in reading order: text blocks, and table cells row by row.
    pub fn texts(&self) -> impl Iterator<Item = (Loc, &RichText)> {
        self.blocks().iter().enumerate().flat_map(
            |(index, block)| -> Box<dyn Iterator<Item = (Loc, &RichText)> + '_> {
                match block.as_ref() {
                    Block::Text(t) => Box::new(std::iter::once((Loc::block(index), &t.text))),
                    Block::Table(table) => {
                        Box::new(table.rows().iter().enumerate().flat_map(move |(r, row)| {
                            row.iter().enumerate().map(move |(c, cell)| {
                                let (r, c) = (
                                    u16::try_from(r).unwrap_or(u16::MAX),
                                    u16::try_from(c).unwrap_or(u16::MAX),
                                );
                                (Loc::cell(index, r, c), cell)
                            })
                        }))
                    }
                    Block::Divider | Block::Image(_) => Box::new(std::iter::empty()),
                }
            },
        )
    }

    /// All matches of `query`, in reading order.
    pub fn find(&self, query: &str, options: FindOptions) -> Vec<Match> {
        self.texts()
            .flat_map(|(loc, text)| {
                find_in_str(text.as_str(), query, options)
                    .into_iter()
                    .map(move |range| Match { loc, range })
            })
            .collect()
    }
}

/// The marks of the character at `offset`.
fn marks_at(text: &RichText, offset: usize) -> Marks {
    text.run_ranges()
        .find(|(r, _)| r.start <= offset && offset < r.end)
        .map_or_else(|| text.marks_for_insert(offset), |(_, m)| m.clone())
}

/// Replaces `range` of `text` with `replacement`, keeping the formatting of the first replaced
/// character.
fn replace_in(text: &mut RichText, range: Range<usize>, replacement: &str) {
    let marks = marks_at(text, range.start);
    text.delete(range.clone());
    if !replacement.is_empty() {
        text.insert(range.start, replacement, marks);
    }
}

impl Editor {
    /// Replaces each match with `replacement` as one undoable step. Matches must be in reading
    /// order and not overlap, as [`Document::find`] returns them. The caret ends after the last
    /// replacement.
    pub fn replace_matches(&mut self, matches: &[Match], replacement: &str) {
        if matches.is_empty() {
            return;
        }
        self.change(Grouping::Separate, |ed| {
            // Back to front, so earlier ranges stay valid.
            for m in matches.iter().rev() {
                if let Some(text) = ed.doc.text_mut(m.loc)
                    && m.range.end <= text.len()
                    && text.as_str().is_char_boundary(m.range.start)
                    && text.as_str().is_char_boundary(m.range.end)
                {
                    replace_in(text, m.range.clone(), replacement);
                }
            }
            if let Some(last) = matches.last() {
                ed.selection = Selection::caret(Pos {
                    loc: last.loc,
                    offset: last.range.start + replacement.len(),
                });
            }
        });
    }

    /// Whether the selection covers text in more than one table cell, or a table and text
    /// outside it, which [`Editor::replace_selected_lines`] cannot keep apart.
    pub fn selection_crosses_table(&self) -> bool {
        let (start, end) = self.selection.ordered();
        if start.loc == end.loc {
            return false;
        }
        (start.loc.block..=end.loc.block)
            .any(|i| matches!(self.doc.block(i), Some(Block::Table(_))))
    }

    /// Replaces the selected text with `text`, keeping the structure of the selection: line `n`
    /// of `text` replaces line `n` of the selection, so headings, lists and line breaks stay where
    /// they were. Only the part of each line that changed is rewritten, which keeps the
    /// formatting of untouched words at either end.
    ///
    /// Returns false, changing nothing, when `text` does not have the same number of lines as the
    /// selection or the selection crosses a table.
    pub fn replace_selected_lines(&mut self, text: &str) -> bool {
        if self.selection.is_collapsed() || self.selection_crosses_table() {
            return false;
        }
        let text = text.replace("\r\n", "\n");
        let ranges = self.selected_ranges();
        let pieces: Vec<String> = ranges
            .iter()
            .map(|(loc, r)| {
                self.doc
                    .text(*loc)
                    .and_then(|t| t.as_str().get(r.clone()))
                    .unwrap_or("")
                    .to_owned()
            })
            .collect();
        let old_lines: usize = pieces.iter().map(|p| p.split('\n').count()).sum();
        let mut new_lines = text.split('\n');
        if text.split('\n').count() != old_lines {
            return false;
        }
        let replacements: Vec<String> = pieces
            .iter()
            .map(|p| {
                let count = p.split('\n').count();
                new_lines
                    .by_ref()
                    .take(count)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect();

        let first = ranges.first().map(|(loc, r)| Pos {
            loc: *loc,
            offset: r.start,
        });
        let last = ranges
            .last()
            .zip(replacements.last())
            .map(|((loc, r), new)| Pos {
                loc: *loc,
                offset: r.start + new.len(),
            });
        self.change(Grouping::Separate, |ed| {
            for ((loc, range), (old, new)) in
                ranges.iter().zip(pieces.iter().zip(&replacements)).rev()
            {
                let (keep_start, keep_end) = common_ends(old, new);
                if keep_start == old.len() && keep_start == new.len() {
                    continue;
                }
                if let Some(t) = ed.doc.text_mut(*loc) {
                    let from = range.start + keep_start;
                    let to = range.end - keep_end;
                    let middle = &new[keep_start..new.len() - keep_end];
                    if from == to {
                        let marks = t.marks_for_insert(from);
                        t.insert(from, middle, marks);
                    } else {
                        replace_in(t, from..to, middle);
                    }
                }
            }
            if let (Some(anchor), Some(head)) = (first, last) {
                ed.selection = Selection { anchor, head };
            }
        });
        true
    }
}

/// Byte lengths of the longest common prefix and suffix of `a` and `b` that do not overlap and
/// end on character boundaries in both.
fn common_ends(a: &str, b: &str) -> (usize, usize) {
    let prefix = a
        .char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or(a.len().min(b.len()), |((i, _), _)| i);
    let max_suffix = (a.len() - prefix).min(b.len() - prefix);
    let suffix = a[prefix..]
        .chars()
        .rev()
        .zip(b[prefix..].chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .scan(0, |sum, len| {
            *sum += len;
            Some(*sum)
        })
        .take_while(|s| *s <= max_suffix)
        .last()
        .unwrap_or(0);
    (prefix, suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_ignoring_case_with_byte_ranges() {
        let text = "Ärger, ärger und ÄRGER";
        let found = find_in_str(text, "ärger", FindOptions::default());
        assert_eq!(found.len(), 3);
        assert!(
            found
                .iter()
                .all(|r| text[r.clone()].to_lowercase() == "ärger")
        );
    }

    #[test]
    fn match_case_and_whole_word() {
        let text = "cat Cat concat cat_x cat.";
        let exact = FindOptions {
            match_case: true,
            whole_word: false,
        };
        assert_eq!(find_in_str(text, "cat", exact).len(), 4);
        let words = FindOptions {
            match_case: false,
            whole_word: true,
        };
        assert_eq!(find_in_str(text, "cat", words), vec![0..3, 4..7, 21..24]);
    }

    #[test]
    fn common_ends_respects_char_boundaries() {
        assert_eq!(common_ends("i was going", "I was going"), (0, 10));
        assert_eq!(common_ends("abc", "abc"), (3, 0));
        assert_eq!(common_ends("aaa", "aa"), (2, 0));
        assert_eq!(common_ends("café x", "cafe x"), (3, 2));
    }
}
