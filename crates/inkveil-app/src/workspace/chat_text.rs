//! Reads the small part of Markdown that chat replies use: paragraphs, headings, lists, fenced
//! code, **bold** and `code`. Works on text that is still arriving, so an unfinished code block
//! or bold mark never makes the reply jump.

use std::ops::Range;

/// How a piece of a line looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Bold,
    Code,
}

/// Text with its marks removed, and where they applied: sorted, never overlapping.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inline {
    pub text: String,
    pub marks: Vec<(Range<usize>, Mark)>,
}

/// One block of a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Inline),
    Heading(Inline),
    /// A list item: its marker ("•" or "1.") and text.
    Item(String, Inline),
    Code(String),
}

/// The text of `line` after a list marker, and the marker to show.
fn list_item(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim_start();
    for bullet in ["- ", "* ", "+ ", "• "] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            return Some(("•".to_owned(), rest));
        }
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    let rest = trimmed[digits..].strip_prefix(". ")?;
    (digits > 0).then(|| (format!("{}.", &trimmed[..digits]), rest))
}

/// The text of a heading line.
fn heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) {
        line[hashes..].strip_prefix(' ')
    } else {
        None
    }
}

/// Splits a reply into blocks.
pub fn blocks(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut code: Option<Vec<&str>> = None;
    let flush = |paragraph: &mut Vec<&str>, out: &mut Vec<Block>| {
        if !paragraph.is_empty() {
            out.push(Block::Paragraph(inline(&paragraph.join("\n"))));
            paragraph.clear();
        }
    };
    for line in text.lines() {
        let fence = line.trim_start().starts_with("```");
        if let Some(lines) = &mut code {
            if fence {
                out.push(Block::Code(lines.join("\n")));
                code = None;
            } else {
                lines.push(line);
            }
        } else if fence {
            flush(&mut paragraph, &mut out);
            code = Some(Vec::new());
        } else if line.trim().is_empty() {
            flush(&mut paragraph, &mut out);
        } else if let Some(title) = heading(line) {
            flush(&mut paragraph, &mut out);
            out.push(Block::Heading(inline(title.trim())));
        } else if let Some((marker, item)) = list_item(line) {
            flush(&mut paragraph, &mut out);
            out.push(Block::Item(marker, inline(item.trim())));
        } else {
            paragraph.push(line.trim_end());
        }
    }
    flush(&mut paragraph, &mut out);
    if let Some(lines) = code {
        out.push(Block::Code(lines.join("\n")));
    }
    out
}

/// Takes the marks out of a line. A mark that is never closed stays as plain text.
pub fn inline(source: &str) -> Inline {
    let mut result = Inline::default();
    let mut rest = source;
    while !rest.is_empty() {
        let next = [("**", Mark::Bold), ("`", Mark::Code)]
            .into_iter()
            .filter_map(|(open, mark)| {
                let start = rest.find(open)?;
                let body = &rest[start + open.len()..];
                let length = body.find(open).filter(|length| *length > 0)?;
                Some((start, open, length, mark))
            })
            .min_by_key(|(start, ..)| *start);
        let Some((start, open, length, mark)) = next else {
            result.text.push_str(rest);
            break;
        };
        result.text.push_str(&rest[..start]);
        let body = &rest[start + open.len()..][..length];
        let at = result.text.len();
        result.text.push_str(body);
        result.marks.push((at..result.text.len(), mark));
        rest = &rest[start + open.len() * 2 + length..];
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Inline {
        Inline {
            text: text.to_owned(),
            marks: Vec::new(),
        }
    }

    #[test]
    fn marks_are_taken_out_of_the_text() {
        let line = inline("a **bold** and `code` word");
        assert_eq!(line.text, "a bold and code word");
        assert_eq!(line.marks, [(2..6, Mark::Bold), (11..15, Mark::Code)]);
    }

    #[test]
    fn marks_that_are_not_closed_stay_as_text() {
        assert_eq!(inline("still **writing"), plain("still **writing"));
        assert_eq!(inline("a `b"), plain("a `b"));
        assert_eq!(inline("2 ** 3"), plain("2 ** 3"));
    }

    #[test]
    fn marks_work_in_any_language() {
        let line = inline("**çfarë** është");
        assert_eq!(line.text, "çfarë është");
        assert_eq!(line.marks, [(0.."çfarë".len(), Mark::Bold)]);
    }

    #[test]
    fn replies_split_into_blocks() {
        let reply = "# Title\n\nFirst line\nsecond line\n\n- one\n* two\n3. three\n";
        assert_eq!(
            blocks(reply),
            [
                Block::Heading(plain("Title")),
                Block::Paragraph(plain("First line\nsecond line")),
                Block::Item("•".into(), plain("one")),
                Block::Item("•".into(), plain("two")),
                Block::Item("3.".into(), plain("three")),
            ]
        );
    }

    #[test]
    fn code_is_kept_exactly_even_before_it_is_finished() {
        assert_eq!(
            blocks("Run:\n```bash\n  - cargo test\n\n# done\n```\nOk"),
            [
                Block::Paragraph(plain("Run:")),
                Block::Code("  - cargo test\n\n# done".into()),
                Block::Paragraph(plain("Ok")),
            ]
        );
        assert_eq!(blocks("```\nlet a"), [Block::Code("let a".into())]);
    }

    #[test]
    fn numbers_that_are_not_list_items_stay_in_the_paragraph() {
        assert_eq!(
            blocks("3.14 is pi"),
            [Block::Paragraph(plain("3.14 is pi"))]
        );
        assert_eq!(blocks("#hashtag"), [Block::Paragraph(plain("#hashtag"))]);
    }
}
