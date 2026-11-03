//! Editing behaviour through the public API.

#![allow(clippy::unwrap_used)]

use inkveil_doc::{
    Align, Block, Document, Editor, FindOptions, Fragment, ImageBlock, MarkKind, Motion, Pos,
    RichText, Selection, TextKind, Unit,
};
use proptest::prelude::*;

fn texts(ed: &Editor) -> Vec<String> {
    ed.doc()
        .blocks()
        .iter()
        .map(|b| match b.as_ref() {
            Block::Text(t) => t.text.as_str().to_owned(),
            Block::Divider => "---".to_owned(),
            Block::Table(t) => format!("[table {}x{}]", t.row_count(), t.col_count()),
            Block::Image(_) => "[image]".to_owned(),
        })
        .collect()
}

fn kind(ed: &Editor, block: usize) -> TextKind {
    ed.doc().block(block).unwrap().as_text().unwrap().kind
}

fn type_str(ed: &mut Editor, s: &str) {
    for c in s.chars() {
        ed.insert_text(&c.to_string());
    }
}

#[test]
fn typing_and_enter() {
    let mut ed = Editor::default();
    type_str(&mut ed, "hello");
    ed.split_block();
    type_str(&mut ed, "world");
    assert_eq!(texts(&ed), ["hello", "world"]);
    assert_eq!(ed.selection().head, Pos::new(1, 5));
}

#[test]
fn typing_is_one_undo_step() {
    let mut ed = Editor::default();
    type_str(&mut ed, "hello");
    ed.undo();
    assert_eq!(texts(&ed), [""]);
    ed.redo();
    assert_eq!(texts(&ed), ["hello"]);
}

#[test]
fn backspace_joins_blocks() {
    let mut ed = Editor::new(Document::from_plain_text("ab\ncd"));
    ed.set_selection(Selection::caret(Pos::new(1, 0)));
    ed.delete_backward(Unit::Grapheme);
    assert_eq!(texts(&ed), ["abcd"]);
    assert_eq!(ed.selection().head, Pos::new(0, 2));
}

#[test]
fn backspace_on_list_item_removes_bullet_first() {
    let mut ed = Editor::default();
    ed.set_text_kind(TextKind::Bullet);
    type_str(&mut ed, "x");
    ed.move_caret(Motion::BlockStart, false);
    ed.delete_backward(Unit::Grapheme);
    assert_eq!(kind(&ed, 0), TextKind::Paragraph);
    assert_eq!(texts(&ed), ["x"]);
}

#[test]
fn enter_in_empty_list_item_ends_the_list() {
    let mut ed = Editor::default();
    ed.set_text_kind(TextKind::Bullet);
    type_str(&mut ed, "item");
    ed.split_block();
    assert_eq!(kind(&ed, 1), TextKind::Bullet);
    ed.split_block();
    assert_eq!(kind(&ed, 1), TextKind::Paragraph);
    assert_eq!(texts(&ed), ["item", ""]);
}

#[test]
fn enter_after_heading_makes_paragraph() {
    let mut ed = Editor::default();
    ed.set_text_kind(TextKind::Heading(1));
    type_str(&mut ed, "Title");
    ed.split_block();
    assert_eq!(kind(&ed, 0), TextKind::Heading(1));
    assert_eq!(kind(&ed, 1), TextKind::Paragraph);
}

#[test]
fn toggle_bold_on_selection_and_caret() {
    let mut ed = Editor::new(Document::from_plain_text("hello world"));
    ed.set_selection(Selection {
        anchor: Pos::new(0, 0),
        head: Pos::new(0, 5),
    });
    ed.toggle_mark(MarkKind::Bold);
    assert!(ed.is_mark_active(MarkKind::Bold));
    ed.toggle_mark(MarkKind::Bold);
    assert!(!ed.is_mark_active(MarkKind::Bold));

    // With nothing selected, the mark applies to what is typed next.
    ed.set_selection(Selection::caret(Pos::new(0, 11)));
    ed.toggle_mark(MarkKind::Italic);
    type_str(&mut ed, "!");
    let text = &ed.doc().block(0).unwrap().as_text().unwrap().text;
    assert!(text.runs().last().unwrap().marks.italic);
    assert_eq!(text.runs().len(), 2);
}

#[test]
fn selection_delete_across_blocks() {
    let mut ed = Editor::new(Document::from_plain_text("one\ntwo\nthree"));
    ed.set_selection(Selection {
        anchor: Pos::new(0, 1),
        head: Pos::new(2, 2),
    });
    ed.insert_text("X");
    assert_eq!(texts(&ed), ["oXree"]);
    ed.undo();
    assert_eq!(texts(&ed), ["one", "two", "three"]);
}

#[test]
fn copy_paste_multiple_blocks() {
    let mut ed = Editor::new(Document::from_plain_text("abc\ndef"));
    ed.set_selection(Selection {
        anchor: Pos::new(0, 1),
        head: Pos::new(1, 2),
    });
    let fragment = ed.copy();
    assert_eq!(fragment.plain_text(), "bc\nde");
    ed.set_selection(Selection::caret(Pos::new(1, 3)));
    ed.paste(&fragment);
    assert_eq!(texts(&ed), ["abc", "defbc", "de"]);
    assert_eq!(ed.selection().head, Pos::new(2, 2));
}

#[test]
fn paste_plain_multiline() {
    let mut ed = Editor::new(Document::from_plain_text("[]"));
    ed.set_selection(Selection::caret(Pos::new(0, 1)));
    ed.insert_text("a\r\nb\nc");
    assert_eq!(texts(&ed), ["[a", "b", "c]"]);
    assert_eq!(ed.selection().head, Pos::new(2, 1));
}

#[test]
fn paste_inline_keeps_block() {
    let mut ed = Editor::new(Document::from_plain_text("ac"));
    ed.set_selection(Selection::caret(Pos::new(0, 1)));
    ed.paste(&Fragment::from_plain_text("b"));
    assert_eq!(texts(&ed), ["abc"]);
}

#[test]
fn divider_insert_and_delete() {
    let mut ed = Editor::new(Document::from_plain_text("abcd"));
    ed.set_selection(Selection::caret(Pos::new(0, 2)));
    ed.insert_divider();
    assert_eq!(texts(&ed), ["ab", "---", "cd"]);
    assert_eq!(ed.selection().head, Pos::new(2, 0));
    ed.delete_backward(Unit::Grapheme);
    assert_eq!(texts(&ed), ["ab", "cd"]);
}

#[test]
fn table_editing() {
    let mut ed = Editor::default();
    ed.insert_table(2, 2);
    assert_eq!(texts(&ed), ["", "[table 2x2]", ""]);
    assert_eq!(ed.selection().head, Pos::in_cell(1, 0, 0, 0));
    type_str(&mut ed, "a");
    ed.table_next_cell();
    type_str(&mut ed, "b");
    ed.table_next_cell();
    ed.table_next_cell();
    ed.table_next_cell(); // past the last cell: adds a row
    assert_eq!(texts(&ed)[1], "[table 3x2]");
    ed.table_delete_col();
    assert_eq!(texts(&ed)[1], "[table 3x1]");
    ed.table_delete();
    assert_eq!(texts(&ed), ["", ""]);
}

#[test]
fn enter_in_code_block_inserts_newline() {
    let mut ed = Editor::default();
    ed.set_text_kind(TextKind::Code);
    type_str(&mut ed, "a");
    ed.split_block();
    type_str(&mut ed, "b");
    assert_eq!(texts(&ed), ["a\nb"]);
}

#[test]
fn block_formatting_and_toggle_back() {
    let mut ed = Editor::new(Document::from_plain_text("a\nb"));
    ed.select_all();
    ed.set_text_kind(TextKind::Numbered);
    assert_eq!(kind(&ed, 0), TextKind::Numbered);
    assert_eq!(kind(&ed, 1), TextKind::Numbered);
    ed.set_text_kind(TextKind::Numbered);
    assert_eq!(kind(&ed, 1), TextKind::Paragraph);
    ed.set_align(Align::Center);
    assert_eq!(ed.current_text_block().unwrap().align, Align::Center);
    ed.indent();
    ed.indent();
    ed.outdent();
    assert_eq!(ed.current_text_block().unwrap().indent, 1);
}

#[test]
fn markdown_shortcuts_at_line_start() {
    for (typed, expected) in [
        ("- ", TextKind::Bullet),
        ("1. ", TextKind::Numbered),
        ("[] ", TextKind::Task { checked: false }),
        ("## ", TextKind::Heading(2)),
        ("> ", TextKind::Quote),
    ] {
        let mut ed = Editor::default();
        type_str(&mut ed, typed);
        assert_eq!(kind(&ed, 0), expected, "{typed:?}");
        assert_eq!(texts(&ed), [""]);
        // Undo brings back the typed characters as plain text.
        ed.undo();
        assert_eq!(kind(&ed, 0), TextKind::Paragraph);
        assert_eq!(texts(&ed), [typed]);
    }
    // Not at the start of a line: no conversion.
    let mut ed = Editor::default();
    type_str(&mut ed, "a - b");
    assert_eq!(kind(&ed, 0), TextKind::Paragraph);
}

#[test]
fn replacing_the_document_is_undoable() {
    let mut ed = Editor::new(Document::from_plain_text("current\ntext"));
    ed.replace_document(Document::from_plain_text("older version"));
    assert_eq!(texts(&ed), ["older version"]);
    assert_eq!(ed.selection().head, Pos::new(0, 0));
    ed.undo();
    assert_eq!(texts(&ed), ["current", "text"]);
    ed.redo();
    assert_eq!(texts(&ed), ["older version"]);
}

#[test]
fn replace_all_is_one_undo_step_and_keeps_formatting() {
    let mut ed = Editor::new(Document::from_plain_text(
        "teh cat
teh dog and teh end",
    ));
    ed.set_selection(Selection {
        anchor: Pos::new(0, 0),
        head: Pos::new(0, 3),
    });
    ed.toggle_mark(MarkKind::Bold);
    let matches = ed.doc().find("TEH", FindOptions::default());
    assert_eq!(matches.len(), 3);
    ed.replace_matches(&matches, "the");
    assert_eq!(texts(&ed), ["the cat", "the dog and the end"]);
    // The first word was bold and stays bold.
    ed.set_selection(Selection {
        anchor: Pos::new(0, 0),
        head: Pos::new(0, 3),
    });
    assert!(ed.is_mark_active(MarkKind::Bold));
    ed.undo();
    assert_eq!(texts(&ed), ["teh cat", "teh dog and teh end"]);
}

#[test]
fn replacing_selected_lines_keeps_block_structure() {
    let mut ed = Editor::new(Document::from_plain_text(
        "intro: i has a idea
second line
last",
    ));
    ed.set_selection(Selection {
        anchor: Pos::new(0, 7),
        head: Pos::new(1, 6),
    });
    ed.set_text_kind(TextKind::Bullet);
    assert!(ed.replace_selected_lines(
        "I have an idea
Second"
    ));
    assert_eq!(texts(&ed), ["intro: I have an idea", "Second line", "last"]);
    assert_eq!(kind(&ed, 0), TextKind::Bullet);
    assert_eq!(kind(&ed, 1), TextKind::Bullet);
    assert_eq!(ed.selection().ordered(), (Pos::new(0, 7), Pos::new(1, 6)));
    // A different number of lines is refused.
    assert!(!ed.replace_selected_lines("one line only"));
    ed.undo();
    assert_eq!(texts(&ed), ["intro: i has a idea", "second line", "last"]);
}

#[test]
fn typing_at_several_carets_is_one_undo_step() {
    let mut ed = Editor::new(Document::from_plain_text("one two\nthree"));
    ed.set_selection(Selection::caret(Pos::new(0, 3)));
    ed.toggle_caret(Pos::new(0, 7));
    ed.toggle_caret(Pos::new(1, 5));
    assert!(ed.has_multiple_carets());
    type_str(&mut ed, "!");
    assert_eq!(texts(&ed), ["one! two!", "three!"]);
    ed.delete_backward(Unit::Grapheme);
    ed.delete_backward(Unit::Grapheme);
    assert_eq!(texts(&ed), ["on tw", "thre"]);
    ed.undo();
    assert_eq!(texts(&ed), ["one two", "three"]);
    assert!(!ed.has_multiple_carets());
}

#[test]
fn carets_move_together_and_toggle_off() {
    let mut ed = Editor::new(Document::from_plain_text("abc\nabc"));
    ed.set_selection(Selection::caret(Pos::new(0, 1)));
    ed.toggle_caret(Pos::new(1, 1));
    ed.move_caret(Motion::Right, false);
    let heads: Vec<Pos> = ed.selections().iter().map(|s| s.head).collect();
    assert!(heads.contains(&Pos::new(0, 2)) && heads.contains(&Pos::new(1, 2)));
    // Clicking an existing caret removes it.
    ed.toggle_caret(Pos::new(1, 2));
    assert!(!ed.has_multiple_carets());
}

#[test]
fn next_occurrence_selects_word_then_adds_matches() {
    let mut ed = Editor::new(Document::from_plain_text("cat dog cat\ncat"));
    ed.set_selection(Selection::caret(Pos::new(0, 1)));
    assert!(ed.select_next_occurrence());
    assert_eq!(ed.selected_plain_text(), "cat");
    assert!(ed.select_next_occurrence());
    assert!(ed.select_next_occurrence());
    assert_eq!(ed.selections().len(), 3);
    assert!(!ed.select_next_occurrence());
    type_str(&mut ed, "cow");
    assert_eq!(texts(&ed), ["cow dog cow", "cow"]);
}

#[test]
fn duplicate_lines_copies_blocks_below() {
    let mut ed = Editor::new(Document::from_plain_text("first\nsecond\nthird"));
    ed.set_selection(Selection::caret(Pos::new(1, 2)));
    ed.set_text_kind(TextKind::Bullet);
    ed.duplicate_lines();
    assert_eq!(texts(&ed), ["first", "second", "second", "third"]);
    assert_eq!(kind(&ed, 2), TextKind::Bullet);
    assert_eq!(ed.selection().head, Pos::new(2, 2));
    ed.undo();
    assert_eq!(texts(&ed), ["first", "second", "third"]);
}

#[test]
fn next_occurrence_reaches_every_line() {
    let mut ed = Editor::new(Document::from_plain_text("asd\nasd\nasd\nasd\nasd"));
    ed.set_selection(Selection::caret(Pos::new(0, 1)));
    for _ in 0..5 {
        ed.select_next_occurrence();
    }
    let mut lines: Vec<usize> = ed.selections().iter().map(|s| s.head.loc.block).collect();
    lines.sort_unstable();
    assert_eq!(lines, [0, 1, 2, 3, 4]);
    type_str(&mut ed, "x");
    assert_eq!(texts(&ed), ["x", "x", "x", "x", "x"]);
}

#[test]
fn next_occurrence_with_touching_matches() {
    let mut ed = Editor::new(Document::from_plain_text("abab\nab"));
    ed.set_selection(Selection {
        anchor: Pos::new(0, 0),
        head: Pos::new(0, 2),
    });
    assert!(ed.select_next_occurrence());
    assert!(ed.select_next_occurrence());
    assert_eq!(ed.selections().len(), 3);
    type_str(&mut ed, "X");
    assert_eq!(texts(&ed), ["XX", "X"]);
}

#[test]
fn next_occurrence_of_a_word_skips_longer_words() {
    let mut ed = Editor::new(Document::from_plain_text("asdasd\nasd\nasd\nasd"));
    ed.set_selection(Selection::caret(Pos::new(1, 1)));
    for _ in 0..6 {
        ed.select_next_occurrence();
    }
    let mut lines: Vec<usize> = ed.selections().iter().map(|s| s.head.loc.block).collect();
    lines.sort_unstable();
    assert_eq!(lines, [1, 2, 3]);
    type_str(&mut ed, "x");
    assert_eq!(texts(&ed), ["asdasd", "x", "x", "x"]);
}

#[test]
fn carets_at_the_end_of_every_selected_line() {
    let mut ed = Editor::new(Document::from_plain_text("one\ntwo\nthree"));
    ed.set_selection(Selection {
        anchor: Pos::new(0, 1),
        head: Pos::new(2, 2),
    });
    ed.carets_at_line_ends();
    assert_eq!(ed.selections().len(), 3);
    type_str(&mut ed, ";");
    assert_eq!(texts(&ed), ["one;", "two;", "three;"]);
}

#[test]
fn column_select_puts_a_caret_on_every_line() {
    let mut ed = Editor::new(Document::from_plain_text("alpha\nb\ngamma\ndelta\nnot me"));
    // Caret on line 3 column 2, Shift+Alt+click on line 0 column 2.
    ed.column_select(Pos::new(3, 2), Pos::new(0, 2));
    assert_eq!(ed.selections().len(), 4);
    assert_eq!(ed.selection().head, Pos::new(0, 2));
    type_str(&mut ed, "|");
    assert_eq!(texts(&ed), ["al|pha", "b|", "ga|mma", "de|lta", "not me"]);
}

#[test]
fn column_select_selects_a_box() {
    let mut ed = Editor::new(Document::from_plain_text("abcdef\nab\nabcdef"));
    ed.column_select(Pos::new(0, 1), Pos::new(2, 4));
    type_str(&mut ed, "X");
    assert_eq!(texts(&ed), ["aXef", "aX", "aXef"]);
}

#[test]
fn carets_added_on_every_line_all_type() {
    let mut ed = Editor::new(Document::from_plain_text("a\nb\nc\nd"));
    ed.set_selection(Selection::caret(Pos::new(0, 1)));
    for line in 1..4 {
        ed.add_caret(Pos::new(line, 1));
    }
    assert_eq!(ed.selections().len(), 4);
    type_str(&mut ed, "!");
    assert_eq!(texts(&ed), ["a!", "b!", "c!", "d!"]);
}

fn picture() -> ImageBlock {
    ImageBlock {
        blob: [7; 16],
        pixel_width: 800,
        pixel_height: 600,
        width_percent: 100,
        align: Align::Left,
    }
}

#[test]
fn images_sit_between_lines_and_can_be_resized_and_removed() {
    let mut ed = Editor::new(Document::from_plain_text("before after"));
    ed.set_selection(Selection::caret(Pos::new(0, 7)));
    ed.insert_image(picture());
    // The line is not split: the picture goes below it, the caret to the line after it.
    assert_eq!(ed.doc().len(), 3);
    assert!(ed.doc().block(1).unwrap().as_image().is_some());
    assert_eq!(texts(&ed)[0], "before after");
    assert_eq!(texts(&ed)[2], "");
    assert_eq!(ed.selection().head, Pos::new(2, 0));
    // The stored form keeps the picture.
    let decoded = Document::decode(&ed.doc().encode()).unwrap();
    assert_eq!(decoded, *ed.doc());

    ed.set_image_layout(1, 40, Align::Center);
    ed.set_image_layout(1, 30, Align::Center);
    let image = ed.doc().block(1).unwrap().as_image().unwrap().clone();
    assert_eq!((image.width_percent, image.align), (30, Align::Center));
    ed.undo();
    assert_eq!(
        ed.doc().block(1).unwrap().as_image().unwrap().width_percent,
        100
    );

    ed.remove_object(1);
    assert!(ed.doc().blocks().iter().all(|b| b.as_image().is_none()));
    ed.undo();
    assert!(ed.doc().block(1).unwrap().as_image().is_some());
    // Backspace at the start of the line after a picture removes the picture.
    ed.set_selection(Selection::caret(Pos::new(2, 0)));
    ed.delete_backward(Unit::Grapheme);
    assert!(ed.doc().blocks().iter().all(|b| b.as_image().is_none()));
}

#[test]
fn an_image_on_an_empty_line_takes_its_place() {
    let mut ed = Editor::new(Document::from_plain_text("text\n\nmore"));
    ed.set_selection(Selection::caret(Pos::new(1, 0)));
    ed.insert_image(picture());
    assert_eq!(texts(&ed), ["text", "[image]", "", "more"]);
}

#[test]
fn task_toggle() {
    let mut ed = Editor::default();
    ed.set_text_kind(TextKind::Task { checked: false });
    ed.toggle_task(0);
    assert_eq!(kind(&ed, 0), TextKind::Task { checked: true });
}

#[test]
fn word_movement_and_deletion() {
    let mut ed = Editor::new(Document::from_plain_text("hello big world"));
    ed.move_caret(Motion::DocEnd, false);
    ed.delete_backward(Unit::Word);
    assert_eq!(texts(&ed), ["hello big "]);
    ed.move_caret(Motion::WordLeft, false);
    assert_eq!(ed.selection().head.offset, 6);
}

#[test]
fn arrows_cross_blocks_and_skip_dividers() {
    let mut ed = Editor::new(Document::from_blocks([
        Block::Text(inkveil_doc::TextBlock::paragraph(RichText::plain("a"))),
        Block::Divider,
        Block::Text(inkveil_doc::TextBlock::paragraph(RichText::plain("b"))),
    ]));
    ed.move_caret(Motion::DocEnd, false);
    ed.move_caret(Motion::Left, false);
    ed.move_caret(Motion::Left, false);
    assert_eq!(ed.selection().head, Pos::new(0, 1));
    ed.move_caret(Motion::Right, false);
    assert_eq!(ed.selection().head, Pos::new(2, 0));
}

// ----- Randomized ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Cmd {
    Type(String),
    Enter,
    SoftBreak,
    Backspace(bool),
    Delete(bool),
    Move(u8, bool),
    Select(usize, usize, usize, usize),
    Mark(u8),
    Kind(u8),
    Indent(bool),
    Divider,
    Table,
    NextCell,
    CopyPaste(usize, usize),
    Undo,
    Redo,
}

fn cmd() -> impl Strategy<Value = Cmd> {
    prop_oneof![
        4 => "[a-zë ]{1,4}".prop_map(Cmd::Type),
        2 => Just(Cmd::Enter),
        1 => Just(Cmd::SoftBreak),
        2 => any::<bool>().prop_map(Cmd::Backspace),
        1 => any::<bool>().prop_map(Cmd::Delete),
        2 => (0u8..8, any::<bool>()).prop_map(|(m, e)| Cmd::Move(m, e)),
        2 => (any::<usize>(), any::<usize>(), any::<usize>(), any::<usize>())
            .prop_map(|(a, b, c, d)| Cmd::Select(a, b, c, d)),
        1 => (0u8..5).prop_map(Cmd::Mark),
        1 => (0u8..7).prop_map(Cmd::Kind),
        1 => any::<bool>().prop_map(Cmd::Indent),
        1 => Just(Cmd::Divider),
        1 => Just(Cmd::Table),
        1 => Just(Cmd::NextCell),
        1 => (any::<usize>(), any::<usize>()).prop_map(|(a, b)| Cmd::CopyPaste(a, b)),
        1 => Just(Cmd::Undo),
        1 => Just(Cmd::Redo),
    ]
}

fn run(ed: &mut Editor, cmd: &Cmd) {
    let blocks = ed.doc().len();
    match cmd {
        Cmd::Type(s) => ed.insert_text(s),
        Cmd::Enter => ed.split_block(),
        Cmd::SoftBreak => ed.insert_soft_break(),
        Cmd::Backspace(w) => ed.delete_backward(if *w { Unit::Word } else { Unit::Grapheme }),
        Cmd::Delete(w) => ed.delete_forward(if *w { Unit::Word } else { Unit::Grapheme }),
        Cmd::Move(m, extend) => {
            let motion = [
                Motion::Left,
                Motion::Right,
                Motion::WordLeft,
                Motion::WordRight,
                Motion::BlockStart,
                Motion::BlockEnd,
                Motion::DocStart,
                Motion::DocEnd,
            ][usize::from(*m)];
            ed.move_caret(motion, *extend);
        }
        Cmd::Select(a, b, c, d) => ed.set_selection(Selection {
            anchor: Pos::new(a % blocks, *b),
            head: Pos::new(c % blocks, *d),
        }),
        Cmd::Mark(m) => ed.toggle_mark(
            [
                MarkKind::Bold,
                MarkKind::Italic,
                MarkKind::Underline,
                MarkKind::Strike,
                MarkKind::Code,
            ][usize::from(*m)],
        ),
        Cmd::Kind(k) => ed.set_text_kind(
            [
                TextKind::Paragraph,
                TextKind::Heading(1),
                TextKind::Bullet,
                TextKind::Numbered,
                TextKind::Task { checked: false },
                TextKind::Quote,
                TextKind::Code,
            ][usize::from(*k)],
        ),
        Cmd::Indent(i) => {
            if *i {
                ed.indent();
            } else {
                ed.outdent();
            }
        }
        Cmd::Divider => ed.insert_divider(),
        Cmd::Table => ed.insert_table(2, 2),
        Cmd::NextCell => ed.table_next_cell(),
        Cmd::CopyPaste(a, b) => {
            let fragment = ed.copy();
            ed.set_selection(Selection::caret(Pos::new(a % blocks, *b)));
            ed.paste(&fragment);
        }
        Cmd::Undo => ed.undo(),
        Cmd::Redo => ed.redo(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// Any command sequence keeps the document valid, the selection inside it, and undoing
    /// everything returns the original document exactly.
    #[test]
    fn random_editing_is_safe_and_undoable(cmds in proptest::collection::vec(cmd(), 1..60)) {
        let original = Document::from_plain_text("first line\nsecond line");
        let mut ed = Editor::new(original.clone());
        for c in &cmds {
            run(&mut ed, c);
            prop_assert!(ed.doc().is_valid(), "invalid after {c:?}");
            let sel = ed.selection();
            prop_assert_eq!(ed.doc().clamp(sel.head), sel.head);
            prop_assert_eq!(ed.doc().clamp(sel.anchor), sel.anchor);
        }
        let encoded = ed.doc().encode();
        prop_assert_eq!(&Document::decode(&encoded).unwrap(), ed.doc());

        while ed.can_undo() {
            ed.undo();
        }
        prop_assert_eq!(ed.doc(), &original);
    }
}
