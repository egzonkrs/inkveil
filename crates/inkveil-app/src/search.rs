//! Searching the text of every note, for the sidebar search.
//!
//! Nothing is indexed: each search decrypts the notes one at a time on the storage thread and
//! looks through their text, so no searchable copy of the notes ever exists on disk.

use std::ops::Range;

use inkveil_doc::{Document, FindOptions, find_in_str};
use inkveil_store::NoteId;

/// Characters of context shown before and after a match.
const BEFORE: usize = 18;
const AFTER: usize = 90;

/// Where a note's text matched the search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyHit {
    pub id: NoteId,
    /// A short excerpt around the first match.
    pub snippet: String,
    /// The match within `snippet`, in bytes.
    pub highlight: Range<usize>,
    /// How many times the search occurs in the note.
    pub count: usize,
}

/// Looks for `query` (ignoring case) in a note's text.
pub fn search_doc(id: NoteId, doc: &Document, query: &str) -> Option<BodyHit> {
    let text = doc.plain_text();
    let found = find_in_str(&text, query, FindOptions::default());
    let first = found.first()?.clone();
    let (snippet, highlight) = excerpt(&text, first);
    Some(BodyHit {
        id,
        snippet,
        highlight,
        count: found.len(),
    })
}

/// The line around `found`, cut to a few words either side, with the match position in it.
fn excerpt(text: &str, found: Range<usize>) -> (String, Range<usize>) {
    let line_start = text[..found.start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[found.end..]
        .find('\n')
        .map_or(text.len(), |i| found.end + i);
    let start = text[line_start..found.start]
        .char_indices()
        .rev()
        .nth(BEFORE - 1)
        .map_or(line_start, |(i, _)| line_start + i);
    let end = text[found.end..line_end]
        .char_indices()
        .nth(AFTER)
        .map_or(line_end, |(i, _)| found.end + i);
    let lead = if start > line_start { "…" } else { "" };
    let tail = if end < line_end { "…" } else { "" };
    let body = text[start..end].replace('\t', " ");
    let snippet = format!("{lead}{body}{tail}");
    let offset = lead.len() + (found.start - start);
    (snippet, offset..offset + (found.end - found.start))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn finds_text_and_cuts_a_snippet_around_it() {
        let doc = Document::from_plain_text(
            "Shopping\nThe quick brown fox jumps over the lazy dog near the old river bank today",
        );
        let id = NoteId::generate().unwrap();
        let hit = search_doc(id, &doc, "LAZY").unwrap();
        assert_eq!(hit.count, 1);
        assert_eq!(&hit.snippet[hit.highlight.clone()], "lazy");
        assert!(hit.snippet.starts_with('…'));
        assert!(search_doc(id, &doc, "missing").is_none());
    }
}
