use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::Marks;

/// A run of text sharing the same marks. `len` is in UTF-8 bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// Length in bytes.
    pub len: usize,
    /// Formatting of the run.
    pub marks: Marks,
}

/// Text with inline formatting.
///
/// Invariants, upheld by every method:
/// - the run lengths add up to `text.len()`, and every run boundary is a char boundary;
/// - no run is empty;
/// - neighbouring runs have different marks.
///
/// Offsets are UTF-8 byte offsets and must lie on char boundaries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RichText {
    text: String,
    runs: Vec<Run>,
}

impl RichText {
    /// Empty text.
    pub fn new() -> Self {
        Self::default()
    }

    /// Unformatted text.
    pub fn plain(text: impl Into<String>) -> Self {
        Self::styled(text, Marks::NONE)
    }

    /// Text with one set of marks throughout.
    pub fn styled(text: impl Into<String>, marks: Marks) -> Self {
        let text = text.into();
        let runs = if text.is_empty() {
            Vec::new()
        } else {
            vec![Run {
                len: text.len(),
                marks,
            }]
        };
        Self { text, runs }
    }

    /// The text without formatting.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Whether the text is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The formatting runs, in order.
    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    /// Each run with its byte range.
    pub fn run_ranges(&self) -> impl Iterator<Item = (Range<usize>, &Marks)> {
        let mut start = 0;
        self.runs.iter().map(move |run| {
            let range = start..start + run.len;
            start = range.end;
            (range, &run.marks)
        })
    }

    /// The marks text typed at `offset` should get: those of the character before it, or of the
    /// first character when `offset` is 0. A link does not extend past its end.
    pub fn marks_for_insert(&self, offset: usize) -> Marks {
        let mut found = None;
        for (range, marks) in self.run_ranges() {
            if offset > range.start && offset <= range.end {
                let mut marks = marks.clone();
                if offset == range.end {
                    marks.link = None;
                }
                return marks;
            }
            if offset == 0 && found.is_none() {
                found = Some(marks.clone());
            }
        }
        let mut marks = found.unwrap_or_default();
        marks.link = None;
        marks
    }

    /// Whether every character in `range` satisfies `pred`. An empty range checks the marks a
    /// typed character would get.
    pub fn all_marks(&self, range: Range<usize>, pred: impl Fn(&Marks) -> bool) -> bool {
        if range.is_empty() {
            return pred(&self.marks_for_insert(range.start));
        }
        self.run_ranges()
            .filter(|(r, _)| r.start < range.end && r.end > range.start)
            .all(|(_, m)| pred(m))
    }

    /// Inserts `text` with `marks` at `offset`.
    pub fn insert(&mut self, offset: usize, text: &str, marks: Marks) {
        self.insert_rich(offset, &Self::styled(text, marks));
    }

    /// Inserts formatted text at `offset`.
    pub fn insert_rich(&mut self, offset: usize, other: &Self) {
        if other.is_empty() {
            return;
        }
        let mut tail = self.split_off(offset);
        self.append(other.clone());
        self.append(std::mem::take(&mut tail));
    }

    /// Removes the text in `range`.
    pub fn delete(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let tail = self.split_off(range.end);
        self.split_off(range.start);
        self.append(tail);
    }

    /// A copy of the text in `range`, with its formatting.
    pub fn slice(&self, range: Range<usize>) -> Self {
        let mut copy = self.clone();
        copy.split_off(range.end);
        copy.split_off(range.start)
    }

    /// Splits at `offset`, keeping the start and returning the rest.
    ///
    /// # Panics
    ///
    /// If `offset` is not a char boundary of the text.
    pub fn split_off(&mut self, offset: usize) -> Self {
        assert!(
            self.text.is_char_boundary(offset),
            "offset {offset} is not a char boundary"
        );
        let tail_text = self.text.split_off(offset);
        let mut head_runs = Vec::new();
        let mut tail_runs = Vec::new();
        let mut start = 0;
        for run in self.runs.drain(..) {
            let end = start + run.len;
            if end <= offset {
                head_runs.push(run);
            } else if start >= offset {
                tail_runs.push(run);
            } else {
                head_runs.push(Run {
                    len: offset - start,
                    marks: run.marks.clone(),
                });
                tail_runs.push(Run {
                    len: end - offset,
                    marks: run.marks,
                });
            }
            start = end;
        }
        self.runs = head_runs;
        Self {
            text: tail_text,
            runs: tail_runs,
        }
    }

    /// Appends `other` to the end.
    pub fn append(&mut self, other: Self) {
        self.text.push_str(&other.text);
        for run in other.runs {
            self.push_run(run);
        }
    }

    /// Applies `f` to the marks of every character in `range`.
    pub fn update_marks(&mut self, range: Range<usize>, f: impl Fn(&mut Marks)) {
        if range.is_empty() {
            return;
        }
        let tail = self.split_off(range.end);
        let mut middle = self.split_off(range.start);
        for run in &mut middle.runs {
            f(&mut run.marks);
        }
        let runs = std::mem::take(&mut middle.runs);
        middle.runs = Vec::with_capacity(runs.len());
        for run in runs {
            middle.push_run(run);
        }
        self.append(middle);
        self.append(tail);
    }

    fn push_run(&mut self, run: Run) {
        if run.len == 0 {
            return;
        }
        match self.runs.last_mut() {
            Some(last) if last.marks == run.marks => last.len += run.len,
            _ => self.runs.push(run),
        }
    }

    /// Checks the invariants. Used by tests and debug assertions.
    pub fn is_valid(&self) -> bool {
        let mut offset = 0;
        for (i, run) in self.runs.iter().enumerate() {
            if run.len == 0 || (i > 0 && self.runs[i - 1].marks == run.marks) {
                return false;
            }
            offset += run.len;
            if !self.text.is_char_boundary(offset) {
                return false;
            }
        }
        offset == self.text.len()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn bold() -> Marks {
        Marks {
            bold: true,
            ..Marks::NONE
        }
    }

    #[test]
    fn insert_and_merge_runs() {
        let mut t = RichText::plain("hello");
        t.insert(5, " world", Marks::NONE);
        assert_eq!(t.runs().len(), 1);
        t.insert(0, ">", bold());
        assert_eq!(t.as_str(), ">hello world");
        assert_eq!(t.runs().len(), 2);
        assert!(t.is_valid());
    }

    #[test]
    fn update_marks_splits_and_merges() {
        let mut t = RichText::plain("abcdef");
        t.update_marks(2..4, |m| m.bold = true);
        assert_eq!(t.runs().len(), 3);
        t.update_marks(0..6, |m| m.bold = false);
        assert_eq!(t.runs().len(), 1);
        assert!(t.is_valid());
    }

    #[test]
    fn delete_across_runs() {
        let mut t = RichText::plain("abc");
        t.insert(3, "def", bold());
        t.insert(6, "ghi", Marks::NONE);
        t.delete(2..7);
        assert_eq!(t.as_str(), "abhi");
        assert_eq!(t.runs().len(), 1);
        assert!(t.is_valid());
    }

    #[test]
    fn marks_for_insert_inherits_from_left() {
        let mut t = RichText::plain("ab");
        t.insert(2, "cd", bold());
        assert!(!t.marks_for_insert(1).bold);
        assert!(t.marks_for_insert(4).bold);
        assert!(!t.marks_for_insert(0).bold);
        let link = Marks {
            link: Some("https://example.com".into()),
            ..Marks::NONE
        };
        let linked = RichText::styled("x", link);
        assert_eq!(linked.marks_for_insert(1).link, None);
    }

    #[test]
    fn multibyte_split() {
        let mut t = RichText::plain("ëë");
        let tail = t.split_off(2);
        assert_eq!((t.as_str(), tail.as_str()), ("ë", "ë"));
        assert!(t.is_valid() && tail.is_valid());
    }

    #[derive(Debug, Clone)]
    enum Op {
        Insert(usize, String, bool),
        Delete(usize, usize),
        Bold(usize, usize, bool),
        Split(usize),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (any::<usize>(), "[a-zë ]{0,6}", any::<bool>())
                .prop_map(|(o, s, b)| Op::Insert(o, s, b)),
            (any::<usize>(), any::<usize>()).prop_map(|(a, b)| Op::Delete(a, b)),
            (any::<usize>(), any::<usize>(), any::<bool>()).prop_map(|(a, b, v)| Op::Bold(a, b, v)),
            any::<usize>().prop_map(Op::Split),
        ]
    }

    fn boundary(t: &str, raw: usize) -> usize {
        let mut o = raw % (t.len() + 1);
        while !t.is_char_boundary(o) {
            o -= 1;
        }
        o
    }

    proptest! {
        #[test]
        fn invariants_hold_under_random_edits(ops in proptest::collection::vec(op(), 0..40)) {
            let mut t = RichText::new();
            let mut model = String::new();
            for op in ops {
                match op {
                    Op::Insert(o, s, b) => {
                        let o = boundary(&model, o);
                        t.insert(o, &s, Marks { bold: b, ..Marks::NONE });
                        model.insert_str(o, &s);
                    }
                    Op::Delete(a, b) => {
                        let (a, b) = (boundary(&model, a), boundary(&model, b));
                        let r = a.min(b)..a.max(b);
                        t.delete(r.clone());
                        model.replace_range(r, "");
                    }
                    Op::Bold(a, b, v) => {
                        let (a, b) = (boundary(&model, a), boundary(&model, b));
                        t.update_marks(a.min(b)..a.max(b), |m| m.bold = v);
                    }
                    Op::Split(o) => {
                        let o = boundary(&model, o);
                        let tail = t.split_off(o);
                        prop_assert!(tail.is_valid());
                        t.append(tail);
                    }
                }
                prop_assert!(t.is_valid(), "{t:?}");
                prop_assert_eq!(t.as_str(), model.as_str());
            }
        }
    }
}
