//! Selecting text in the chat. The conversation is drawn as many separate text blocks, so a
//! selection is a pair of places in those blocks and can run across several of them.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

/// A place in the conversation: a byte offset in one of its text blocks, counted from the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPos {
    pub block: usize,
    pub offset: usize,
}

/// A selection, from where the mouse went down to where it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: TextPos,
    pub head: TextPos,
}

impl TextSelection {
    pub fn at(pos: TextPos) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
    }

    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }

    fn ordered(self) -> (TextPos, TextPos) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }

    /// The part of block `block`, `len` bytes long, that is selected.
    pub fn range_in(self, block: usize, len: usize) -> Option<Range<usize>> {
        let (start, end) = self.ordered();
        if block < start.block || block > end.block {
            return None;
        }
        let from = if block == start.block {
            start.offset.min(len)
        } else {
            0
        };
        let to = if block == end.block {
            end.offset.min(len)
        } else {
            len
        };
        (from < to).then_some(from..to)
    }

    /// The selected text, with a blank line between blocks.
    pub fn text<'a>(self, blocks: impl IntoIterator<Item = &'a str>) -> String {
        let parts: Vec<&str> = blocks
            .into_iter()
            .enumerate()
            .filter_map(|(index, text)| Some(&text[self.range_in(index, text.len())?]))
            .collect();
        parts.join("\n\n")
    }
}

/// The word at `offset`, for a double click. Between words it is the space or mark that was hit.
pub fn word_at(text: &str, offset: usize) -> Range<usize> {
    text.split_word_bound_indices()
        .map(|(start, word)| start..start + word.len())
        .find(|word| offset < word.end)
        .unwrap_or(text.len()..text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(block: usize, offset: usize) -> TextPos {
        TextPos { block, offset }
    }

    fn selection(anchor: TextPos, head: TextPos) -> TextSelection {
        TextSelection { anchor, head }
    }

    #[test]
    fn a_selection_inside_one_block() {
        let s = selection(pos(1, 2), pos(1, 5));
        assert_eq!(s.range_in(1, 10), Some(2..5));
        assert_eq!(s.range_in(0, 10), None);
        assert_eq!(s.text(["first", "second one"]), "con");
    }

    #[test]
    fn dragging_upwards_selects_the_same_text() {
        let down = selection(pos(0, 2), pos(2, 3));
        let up = selection(pos(2, 3), pos(0, 2));
        let blocks = ["first", "middle", "last"];
        assert_eq!(down.text(blocks), up.text(blocks));
        assert_eq!(down.text(blocks), "rst\n\nmiddle\n\nlas");
    }

    #[test]
    fn an_empty_selection_has_no_text() {
        let s = TextSelection::at(pos(0, 3));
        assert!(s.is_empty());
        assert_eq!(s.text(["first"]), "");
    }

    #[test]
    fn a_block_that_got_shorter_cannot_break_it() {
        let s = selection(pos(0, 0), pos(0, 50));
        assert_eq!(s.range_in(0, 5), Some(0..5));
    }

    #[test]
    fn a_double_click_finds_the_word() {
        let text = "një fjalë e gjatë";
        assert_eq!(&text[word_at(text, 6)], "fjalë");
        assert_eq!(&text[word_at(text, 0)], "një");
        assert_eq!(&text[word_at(text, 4)], " ");
        assert_eq!(word_at(text, text.len()), text.len()..text.len());
    }
}
