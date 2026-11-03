//! Grapheme and word boundaries for caret movement and deletion.

use unicode_segmentation::UnicodeSegmentation;

/// The grapheme boundary before `offset` (or 0).
pub(crate) fn prev_grapheme(text: &str, offset: usize) -> usize {
    text[..offset]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}

/// The grapheme boundary after `offset` (or the end).
pub(crate) fn next_grapheme(text: &str, offset: usize) -> usize {
    text[offset..]
        .graphemes(true)
        .next()
        .map_or(text.len(), |g| offset + g.len())
}

/// The start of the word before `offset`, skipping whitespace and punctuation first.
pub(crate) fn prev_word(text: &str, offset: usize) -> usize {
    text[..offset]
        .split_word_bound_indices()
        .rev()
        .find(|(_, w)| is_word(w))
        .map_or(0, |(i, _)| i)
}

/// The end of the word after `offset`, skipping whitespace and punctuation first.
pub(crate) fn next_word(text: &str, offset: usize) -> usize {
    text[offset..]
        .split_word_bound_indices()
        .find(|(_, w)| is_word(w))
        .map_or(text.len(), |(i, w)| offset + i + w.len())
}

/// The word around `offset`, for double-click selection.
pub(crate) fn word_at(text: &str, offset: usize) -> (usize, usize) {
    for (i, w) in text.split_word_bound_indices() {
        if offset >= i && offset < i + w.len() || offset == i + w.len() && offset == text.len() {
            return (i, i + w.len());
        }
    }
    (offset, offset)
}

fn is_word(segment: &str) -> bool {
    segment.chars().any(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphemes() {
        let s = "ae\u{301}b"; // "aéb" with a combining accent
        assert_eq!(next_grapheme(s, 1), 4);
        assert_eq!(prev_grapheme(s, 4), 1);
        assert_eq!(prev_grapheme(s, 0), 0);
        assert_eq!(next_grapheme(s, s.len()), s.len());
    }

    #[test]
    fn words() {
        let s = "hello, big world";
        assert_eq!(prev_word(s, s.len()), 11);
        assert_eq!(prev_word(s, 11), 7);
        assert_eq!(prev_word(s, 7), 0);
        assert_eq!(next_word(s, 0), 5);
        assert_eq!(next_word(s, 5), 10);
        assert_eq!(word_at(s, 8), (7, 10));
    }
}
