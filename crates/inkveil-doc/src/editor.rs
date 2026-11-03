//! Editing commands.

use std::{ops::Range, sync::Arc};

use crate::{
    Align, Block, CellRef, Document, FontSize, Loc, Marks, Pos, Rgb, RichText, Selection, Table,
    TextBlock, TextKind,
    block::MAX_INDENT,
    document::diff_blocks,
    history::{Grouping, History},
    text_ops,
};

/// An inline mark that can be switched on and off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkKind {
    /// Bold.
    Bold,
    /// Italic.
    Italic,
    /// Underline.
    Underline,
    /// Strikethrough.
    Strike,
    /// Inline code.
    Code,
}

impl MarkKind {
    fn get(self, marks: &Marks) -> bool {
        match self {
            Self::Bold => marks.bold,
            Self::Italic => marks.italic,
            Self::Underline => marks.underline,
            Self::Strike => marks.strike,
            Self::Code => marks.code,
        }
    }

    fn set(self, marks: &mut Marks, on: bool) {
        match self {
            Self::Bold => marks.bold = on,
            Self::Italic => marks.italic = on,
            Self::Underline => marks.underline = on,
            Self::Strike => marks.strike = on,
            Self::Code => marks.code = on,
        }
    }
}

/// How much Backspace or Delete removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// One user-perceived character.
    Grapheme,
    /// To the previous or next word boundary.
    Word,
}

/// Caret movements that need no layout information. Up/Down and visual line ends are worked out
/// by the view, which then calls [`Editor::set_selection`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One character left.
    Left,
    /// One character right.
    Right,
    /// To the start of the previous word.
    WordLeft,
    /// To the end of the next word.
    WordRight,
    /// To the start of the block.
    BlockStart,
    /// To the end of the block.
    BlockEnd,
    /// To the start of the document.
    DocStart,
    /// To the end of the document.
    DocEnd,
}

/// A copied piece of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    blocks: Vec<Block>,
    /// The fragment is part of a single text and pastes inline.
    inline: bool,
}

impl Fragment {
    /// A fragment of unformatted lines.
    pub fn from_plain_text(text: &str) -> Self {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<&str> = text.split('\n').collect();
        Self {
            inline: lines.len() == 1,
            blocks: lines
                .into_iter()
                .map(|l| Block::Text(TextBlock::paragraph(RichText::plain(l))))
                .collect(),
        }
    }

    /// The fragment as plain text, one line per block.
    pub fn plain_text(&self) -> String {
        Document {
            blocks: self.blocks.iter().cloned().map(Arc::new).collect(),
        }
        .plain_text()
    }
}

/// A document with a selection and undo history. All edits go through here.
#[derive(Debug, Clone)]
pub struct Editor {
    pub(crate) doc: Document,
    pub(crate) selection: Selection,
    /// Marks chosen with nothing selected, applied to the next typed text.
    pub(crate) stored_marks: Option<Marks>,
    pub(crate) history: History,
    version: u64,
    /// Selections besides the primary one, for multiple carets.
    pub(crate) extra: Vec<Selection>,
    /// Set by multi-caret edits so [`Editor::change`] keeps the extra carets.
    pub(crate) keep_extra: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new(Document::new())
    }
}

impl Editor {
    /// Starts editing `doc` with the caret at its start.
    pub fn new(doc: Document) -> Self {
        let start = doc.start();
        Self {
            doc,
            selection: Selection::caret(start),
            stored_marks: None,
            history: History::default(),
            version: 0,
            extra: Vec::new(),
            keep_extra: false,
        }
    }

    /// The document.
    pub fn doc(&self) -> &Document {
        &self.doc
    }

    /// The selection.
    pub fn selection(&self) -> Selection {
        self.selection
    }

    /// Increases with every change to the document (not to the selection).
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Whether there is something to undo.
    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    /// Whether there is something to redo.
    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// The text block the caret is in, if any.
    pub fn current_text_block(&self) -> Option<&TextBlock> {
        let loc = self.selection.head.loc;
        if loc.cell.is_some() {
            return None;
        }
        self.doc.block(loc.block)?.as_text()
    }

    /// Whether the caret is in a table cell.
    pub fn in_table(&self) -> bool {
        self.selection.head.loc.cell.is_some()
    }

    // ----- Selection ---------------------------------------------------------------------------

    /// Replaces the selection. Both ends are clamped to valid positions.
    pub fn set_selection(&mut self, selection: Selection) {
        let selection = Selection {
            anchor: self.doc.clamp(selection.anchor),
            head: self.doc.clamp(selection.head),
        };
        if selection != self.selection || !self.extra.is_empty() {
            self.stored_marks = None;
            self.history.break_group();
        }
        self.selection = selection;
        self.extra.clear();
    }

    /// Moves the caret, or extends the selection when `extend` is true.
    pub fn move_caret(&mut self, motion: Motion, extend: bool) {
        if self.extra.is_empty() {
            let moved = self.moved(self.selection, motion, extend);
            self.set_selection(moved);
        } else {
            let moved = self
                .selections()
                .into_iter()
                .map(|s| self.moved(s, motion, extend))
                .collect();
            self.set_selections(moved);
        }
    }

    /// Where `selection` goes for `motion`.
    fn moved(&self, selection: Selection, motion: Motion, extend: bool) -> Selection {
        let head = selection.head;
        if !extend && !selection.is_collapsed() {
            let (start, end) = selection.ordered();
            match motion {
                Motion::Left => return Selection::caret(start),
                Motion::Right => return Selection::caret(end),
                _ => {}
            }
        }
        let text = self.doc.text(head.loc).map_or("", RichText::as_str);
        let target = match motion {
            Motion::Left if head.offset > 0 => Pos {
                offset: text_ops::prev_grapheme(text, head.offset),
                ..head
            },
            Motion::WordLeft if head.offset > 0 => Pos {
                offset: text_ops::prev_word(text, head.offset),
                ..head
            },
            Motion::Left | Motion::WordLeft => {
                self.prev_loc(head.loc).map_or(head, |loc| self.end_of(loc))
            }
            Motion::Right if head.offset < text.len() => Pos {
                offset: text_ops::next_grapheme(text, head.offset),
                ..head
            },
            Motion::WordRight if head.offset < text.len() => Pos {
                offset: text_ops::next_word(text, head.offset),
                ..head
            },
            Motion::Right | Motion::WordRight => self
                .next_loc(head.loc)
                .map_or(head, |loc| Pos { loc, offset: 0 }),
            Motion::BlockStart => Pos { offset: 0, ..head },
            Motion::BlockEnd => self.end_of(head.loc),
            Motion::DocStart => self.doc.start(),
            Motion::DocEnd => self.doc.end(),
        };
        let anchor = if extend { selection.anchor } else { target };
        Selection {
            anchor,
            head: target,
        }
    }

    /// Selects the whole document.
    pub fn select_all(&mut self) {
        self.set_selection(Selection {
            anchor: self.doc.start(),
            head: self.doc.end(),
        });
    }

    /// Selects the word at `pos` (double click).
    pub fn select_word_at(&mut self, pos: Pos) {
        let pos = self.doc.clamp(pos);
        let text = self.doc.text(pos.loc).map_or("", RichText::as_str);
        let (start, end) = text_ops::word_at(text, pos.offset);
        self.set_selection(Selection {
            anchor: Pos {
                offset: start,
                ..pos
            },
            head: Pos { offset: end, ..pos },
        });
    }

    /// Selects all text at `pos`'s location (triple click).
    pub fn select_text_at(&mut self, pos: Pos) {
        let pos = self.doc.clamp(pos);
        self.set_selection(Selection {
            anchor: Pos { offset: 0, ..pos },
            head: self.end_of(pos.loc),
        });
    }

    /// The selected text as plain text.
    pub fn selected_plain_text(&self) -> String {
        self.copy().plain_text()
    }

    // ----- Typing ------------------------------------------------------------------------------

    /// Types or pastes plain `text` over the selection.
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.extra.is_empty() {
            if self.multi_insert(text) {
                return;
            }
            self.extra.clear();
        }
        let multiline = text.contains(['\n', '\r']);
        let grouping = if self.selection.is_collapsed() && !multiline {
            Grouping::Typing
        } else {
            Grouping::Separate
        };
        self.change(grouping, |ed| {
            let caret = ed.delete_selection_inner();
            let marks = ed.insert_marks(caret);
            if multiline && caret.loc.cell.is_none() && !ed.is_code(caret.loc) {
                let mut fragment = Fragment::from_plain_text(text);
                for block in &mut fragment.blocks {
                    if let Block::Text(t) = block {
                        t.text.update_marks(0..t.text.len(), |m| *m = marks.clone());
                    }
                }
                ed.selection = Selection::caret(ed.insert_fragment_inner(caret, fragment));
            } else {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                ed.insert_at(caret, &text, marks);
            }
        });
        if text == " " {
            self.apply_input_rule();
        }
    }

    /// Markdown-style shortcuts typed at the start of a paragraph: `- `, `* `, `1. `, `[] `,
    /// `[x] `, `# `, `## `, `### `, `> ` and ```` ``` ````. Applied as a separate undo step, so
    /// Undo brings back the typed characters.
    fn apply_input_rule(&mut self) {
        let caret = self.selection.head;
        if !self.selection.is_collapsed() || caret.loc.cell.is_some() {
            return;
        }
        let Some(Block::Text(block)) = self.doc.block(caret.loc.block) else {
            return;
        };
        if block.kind != TextKind::Paragraph {
            return;
        }
        let kind = match &block.text.as_str()[..caret.offset] {
            "- " | "* " => TextKind::Bullet,
            "1. " | "1) " => TextKind::Numbered,
            "[] " | "[ ] " => TextKind::Task { checked: false },
            "[x] " | "[X] " => TextKind::Task { checked: true },
            "# " => TextKind::Heading(1),
            "## " => TextKind::Heading(2),
            "### " => TextKind::Heading(3),
            "> " => TextKind::Quote,
            "``` " => TextKind::Code,
            _ => return,
        };
        let prefix = caret.offset;
        self.change(Grouping::Separate, |ed| {
            if let Some(Block::Text(b)) = ed.doc.block_mut(caret.loc.block) {
                b.text.delete(0..prefix);
                b.kind = kind;
            }
            ed.selection = Selection::caret(Pos::new(caret.loc.block, 0));
        });
    }

    /// Inserts a line break inside the current block (Shift+Enter).
    pub fn insert_soft_break(&mut self) {
        if !self.extra.is_empty() && self.multi_insert("\n") {
            return;
        }
        self.change(Grouping::Separate, |ed| {
            let caret = ed.delete_selection_inner();
            let marks = ed.insert_marks(caret);
            ed.insert_at(caret, "\n", marks);
        });
    }

    /// Splits the block at the caret (Enter).
    ///
    /// In a code block or table cell this inserts a line break. In an empty list item it outdents
    /// the item, or turns it into a paragraph when it is not indented.
    pub fn split_block(&mut self) {
        self.change(Grouping::Separate, |ed| {
            let caret = ed.delete_selection_inner();
            let marks = ed.insert_marks(caret);
            if caret.loc.cell.is_some() || ed.is_code(caret.loc) {
                ed.insert_at(caret, "\n", marks);
                return;
            }
            let Some(Block::Text(current)) = ed.doc.block_mut(caret.loc.block) else {
                return;
            };
            if current.kind.is_list() && current.text.is_empty() {
                if current.indent > 0 {
                    current.indent -= 1;
                } else {
                    current.kind = TextKind::Paragraph;
                }
                return;
            }
            let tail = current.text.split_off(caret.offset);
            let mut next = current.with_text(tail);
            if next.text.is_empty() {
                next.kind = current.kind.continuation();
                if matches!(current.kind, TextKind::Heading(_)) {
                    next.size = FontSize::Normal;
                }
            }
            let at = caret.loc.block + 1;
            ed.doc.blocks.insert(at, Arc::new(Block::Text(next)));
            ed.selection = Selection::caret(Pos::new(at, 0));
            // Text typed at the start of the new block should keep the formatting.
            if marks != Marks::NONE {
                ed.stored_marks = Some(marks);
            }
        });
    }

    /// Backspace.
    pub fn delete_backward(&mut self, unit: Unit) {
        if !self.extra.is_empty() {
            return self.multi_delete(false, unit);
        }
        if !self.selection.is_collapsed() {
            return self.change(Grouping::Separate, |ed| {
                ed.delete_selection_inner();
            });
        }
        let caret = self.selection.head;
        if caret.offset > 0 {
            let text = self.doc.text(caret.loc).map_or("", RichText::as_str);
            let start = match unit {
                Unit::Grapheme => text_ops::prev_grapheme(text, caret.offset),
                Unit::Word => text_ops::prev_word(text, caret.offset),
            };
            return self.change(Grouping::Typing, |ed| {
                if let Some(t) = ed.doc.text_mut(caret.loc) {
                    t.delete(start..caret.offset);
                }
                ed.selection = Selection::caret(Pos {
                    offset: start,
                    ..caret
                });
            });
        }
        if caret.loc.cell.is_some() {
            return;
        }
        self.change(Grouping::Separate, |ed| ed.join_backward(caret.loc.block));
    }

    /// Delete.
    pub fn delete_forward(&mut self, unit: Unit) {
        if !self.extra.is_empty() {
            return self.multi_delete(true, unit);
        }
        if !self.selection.is_collapsed() {
            return self.change(Grouping::Separate, |ed| {
                ed.delete_selection_inner();
            });
        }
        let caret = self.selection.head;
        let text = self.doc.text(caret.loc).map_or("", RichText::as_str);
        if caret.offset < text.len() {
            let end = match unit {
                Unit::Grapheme => text_ops::next_grapheme(text, caret.offset),
                Unit::Word => text_ops::next_word(text, caret.offset),
            };
            return self.change(Grouping::Separate, |ed| {
                if let Some(t) = ed.doc.text_mut(caret.loc) {
                    t.delete(caret.offset..end);
                }
            });
        }
        if caret.loc.cell.is_some() {
            return;
        }
        let next = caret.loc.block + 1;
        match self.doc.block(next) {
            Some(Block::Text(_)) => self.change(Grouping::Separate, |ed| ed.join_backward(next)),
            Some(Block::Divider | Block::Image(_)) => self.change(Grouping::Separate, |ed| {
                ed.doc.blocks.remove(next);
            }),
            _ => {}
        }
    }

    /// Backspace at the start of text block `index`: removes its formatting first, then its
    /// indent, then merges it into the block before.
    fn join_backward(&mut self, index: usize) {
        let Some(Block::Text(block)) = self.doc.block(index) else {
            return;
        };
        if block.kind != TextKind::Paragraph {
            if let Some(Block::Text(b)) = self.doc.block_mut(index) {
                b.kind = TextKind::Paragraph;
            }
            return;
        }
        if block.indent > 0 {
            if let Some(Block::Text(b)) = self.doc.block_mut(index) {
                b.indent -= 1;
            }
            return;
        }
        if index == 0 {
            return;
        }
        match self.doc.block(index - 1) {
            Some(Block::Text(prev)) => {
                let join_at = prev.text.len();
                let moved = self.doc.blocks.remove(index);
                let moved_text = moved.as_text().map(|t| t.text.clone()).unwrap_or_default();
                if let Some(Block::Text(prev)) = self.doc.block_mut(index - 1) {
                    prev.text.append(moved_text);
                }
                self.selection = Selection::caret(Pos::new(index - 1, join_at));
            }
            Some(Block::Divider | Block::Image(_)) => {
                self.doc.blocks.remove(index - 1);
                self.selection = Selection::caret(Pos::new(index - 1, 0));
            }
            Some(Block::Table(table)) => {
                let last = CellRef {
                    row: table.row_count() - 1,
                    col: table.col_count() - 1,
                };
                if block.text.is_empty() {
                    self.doc.blocks.remove(index);
                }
                let loc = Loc {
                    block: index - 1,
                    cell: Some(last),
                };
                self.selection = Selection::caret(self.end_of(loc));
            }
            None => {}
        }
    }

    // ----- Inline formatting -------------------------------------------------------------------

    /// Whether `kind` is on for all selected text (or for text typed at the caret).
    pub fn is_mark_active(&self, kind: MarkKind) -> bool {
        if self.selection.is_collapsed() {
            return kind.get(&self.caret_marks());
        }
        self.selected_ranges()
            .iter()
            .filter(|(_, r)| !r.is_empty())
            .all(|(loc, r)| {
                self.doc
                    .text(*loc)
                    .is_none_or(|t| t.all_marks(r.clone(), |m| kind.get(m)))
            })
    }

    /// The marks text typed at the caret would get.
    pub fn caret_marks(&self) -> Marks {
        self.stored_marks
            .clone()
            .unwrap_or_else(|| self.insert_marks(self.selection.head))
    }

    /// Turns `kind` on for the selection, or off if it is already on everywhere.
    pub fn toggle_mark(&mut self, kind: MarkKind) {
        let on = !self.is_mark_active(kind);
        self.update_marks(|m| kind.set(m, on));
    }

    /// Sets or clears the text color.
    pub fn set_color(&mut self, color: Option<Rgb>) {
        self.update_marks(|m| m.color = color);
    }

    /// Sets or clears the highlight color.
    pub fn set_highlight(&mut self, color: Option<Rgb>) {
        self.update_marks(|m| m.highlight = color);
    }

    /// Links the selected text to `url`, or removes links with `None`.
    pub fn set_link(&mut self, url: Option<&str>) {
        let url: Option<Arc<str>> = url.map(Into::into);
        if self.selection.is_collapsed() {
            return;
        }
        self.update_marks(|m| m.link.clone_from(&url));
    }

    /// Inserts `text` linked to `url` at the caret, replacing the selection.
    pub fn insert_link(&mut self, text: &str, url: &str) {
        if text.is_empty() {
            return;
        }
        self.change(Grouping::Separate, |ed| {
            let caret = ed.delete_selection_inner();
            let mut marks = ed.insert_marks(caret);
            marks.link = Some(url.into());
            ed.insert_at(caret, text, marks);
        });
    }

    /// Removes all inline formatting from the selection.
    pub fn clear_formatting(&mut self) {
        self.update_marks(|m| *m = Marks::NONE);
    }

    fn update_marks(&mut self, f: impl Fn(&mut Marks)) {
        if self.selection.is_collapsed() {
            let mut marks = self.caret_marks();
            f(&mut marks);
            self.stored_marks = Some(marks);
            return;
        }
        let ranges = self.selected_ranges();
        self.change(Grouping::Separate, |ed| {
            for (loc, range) in ranges {
                if let Some(t) = ed.doc.text_mut(loc) {
                    t.update_marks(range, &f);
                }
            }
        });
    }

    // ----- Block formatting --------------------------------------------------------------------

    /// Makes the selected blocks `kind`, or paragraphs if they already all are `kind`.
    pub fn set_text_kind(&mut self, kind: TextKind) {
        let all_same = self
            .selected_text_blocks()
            .all(|(_, b)| b.kind.same_kind(kind));
        let target = if all_same && kind != TextKind::Paragraph {
            TextKind::Paragraph
        } else {
            kind
        };
        self.update_blocks(|b| {
            if !b.kind.same_kind(target) {
                b.kind = target;
            }
        });
    }

    /// Sets the alignment of the selected blocks.
    pub fn set_align(&mut self, align: Align) {
        self.update_blocks(|b| b.align = align);
    }

    /// Sets the text size of the selected blocks.
    pub fn set_size(&mut self, size: FontSize) {
        self.update_blocks(|b| b.size = size);
    }

    /// Indents the selected blocks one level.
    pub fn indent(&mut self) {
        self.update_blocks(|b| b.indent = (b.indent + 1).min(MAX_INDENT));
    }

    /// Outdents the selected blocks one level.
    pub fn outdent(&mut self) {
        self.update_blocks(|b| b.indent = b.indent.saturating_sub(1));
    }

    /// Ticks or unticks the checklist item at block `index`.
    pub fn toggle_task(&mut self, index: usize) {
        if !matches!(
            self.doc.block(index),
            Some(Block::Text(TextBlock {
                kind: TextKind::Task { .. },
                ..
            }))
        ) {
            return;
        }
        self.change(Grouping::Separate, |ed| {
            if let Some(Block::Text(b)) = ed.doc.block_mut(index)
                && let TextKind::Task { checked } = &mut b.kind
            {
                *checked = !*checked;
            }
        });
    }

    fn update_blocks(&mut self, f: impl Fn(&mut TextBlock)) {
        let indices: Vec<usize> = self.selected_text_blocks().map(|(i, _)| i).collect();
        if indices.is_empty() {
            return;
        }
        self.change(Grouping::Separate, |ed| {
            for i in indices {
                if let Some(Block::Text(b)) = ed.doc.blocks.get(i).map(AsRef::as_ref) {
                    let mut updated = b.clone();
                    f(&mut updated);
                    if updated != *b {
                        ed.doc.blocks[i] = Arc::new(Block::Text(updated));
                    }
                }
            }
        });
    }

    fn selected_text_blocks(&self) -> impl Iterator<Item = (usize, &TextBlock)> {
        let (start, end) = self.selection.ordered();
        (start.loc.block..=end.loc.block)
            .filter_map(|i| self.doc.block(i)?.as_text().map(|t| (i, t)))
    }

    // ----- Dividers and tables -----------------------------------------------------------------

    /// Inserts a horizontal divider at the caret.
    pub fn insert_divider(&mut self) {
        self.insert_object(Block::Divider);
    }

    /// Inserts a picture on its own line below the caret's line, without splitting the text.
    /// On an empty line the picture takes that line's place (the empty line stays below it).
    pub fn insert_image(&mut self, image: crate::ImageBlock) {
        if self.in_table() {
            return;
        }
        let (_, end) = self.selection.ordered();
        let index = end.loc.block;
        let empty_line = self
            .doc
            .block(index)
            .and_then(Block::as_text)
            .is_some_and(|t| t.text.is_empty());
        self.change(Grouping::Separate, |ed| {
            let at = if empty_line { index } else { index + 1 };
            ed.doc.blocks.insert(at, Arc::new(Block::Image(image)));
            // The caret goes to the line after the picture (normalizing adds one if needed).
            ed.selection = Selection::caret(Pos::new(at + 1, 0));
        });
    }

    /// Removes the divider or image at `index` (a selected picture and Delete), as one undoable
    /// step. Text blocks and tables are left alone.
    pub fn remove_object(&mut self, index: usize) {
        if !matches!(
            self.doc.block(index),
            Some(Block::Divider | Block::Image(_))
        ) {
            return;
        }
        self.change(Grouping::Separate, |ed| {
            ed.doc.blocks.remove(index);
            ed.selection = Selection::caret(Pos::new(index.min(ed.doc.len() - 1), 0));
        });
    }

    /// Changes how the image at `index` is shown: its width in percent of the text column and
    /// its alignment. Consecutive width changes (dragging the resize handle) are one undo step.
    pub fn set_image_layout(&mut self, index: usize, width_percent: u8, align: Align) {
        let width_percent = width_percent.clamp(crate::ImageBlock::MIN_PERCENT, 100);
        match self.doc.block(index) {
            Some(Block::Image(i)) if i.width_percent != width_percent || i.align != align => {}
            _ => return,
        }
        self.change(Grouping::Typing, |ed| {
            if let Some(Block::Image(image)) = ed.doc.block_mut(index) {
                image.width_percent = width_percent;
                image.align = align;
            }
        });
    }

    /// Inserts an empty table and puts the caret in its first cell.
    pub fn insert_table(&mut self, rows: u16, cols: u16) {
        self.insert_object(Block::Table(Table::new(rows, cols)));
        let (start, _) = self.selection.ordered();
        if let Some(Block::Table(_)) = start
            .loc
            .block
            .checked_sub(1)
            .and_then(|i| self.doc.block(i))
        {
            self.selection = Selection::caret(Pos::in_cell(start.loc.block - 1, 0, 0, 0));
        }
    }

    fn insert_object(&mut self, object: Block) {
        if self.in_table() {
            return;
        }
        self.change(Grouping::Separate, |ed| {
            let caret = ed.delete_selection_inner();
            let index = caret.loc.block;
            let Some(Block::Text(current)) = ed.doc.block_mut(index) else {
                return;
            };
            if caret.offset == 0 {
                ed.doc.blocks.insert(index, Arc::new(object));
                ed.selection = Selection::caret(Pos::new(index + 1, 0));
            } else {
                let tail = current.text.split_off(caret.offset);
                let next = current.with_text(tail);
                ed.doc.blocks.insert(index + 1, Arc::new(object));
                ed.doc.blocks.insert(index + 2, Arc::new(Block::Text(next)));
                ed.selection = Selection::caret(Pos::new(index + 2, 0));
            }
        });
    }

    /// Adds a row below the caret's row.
    pub fn table_insert_row_below(&mut self) {
        self.table_edit(|table, cell| table.insert_row(cell.row + 1).then_some(cell));
    }

    /// Adds a column right of the caret's column.
    pub fn table_insert_col_right(&mut self) {
        self.table_edit(|table, cell| table.insert_col(cell.col + 1).then_some(cell));
    }

    /// Deletes the caret's row.
    pub fn table_delete_row(&mut self) {
        self.table_edit(|table, cell| {
            table.remove_row(cell.row).then(|| CellRef {
                row: cell.row.min(table.row_count() - 1),
                col: cell.col,
            })
        });
    }

    /// Deletes the caret's column.
    pub fn table_delete_col(&mut self) {
        self.table_edit(|table, cell| {
            table.remove_col(cell.col).then(|| CellRef {
                row: cell.row,
                col: cell.col.min(table.col_count() - 1),
            })
        });
    }

    /// Deletes the whole table the caret is in.
    pub fn table_delete(&mut self) {
        let loc = self.selection.head.loc;
        if loc.cell.is_none() {
            return;
        }
        self.change(Grouping::Separate, |ed| {
            ed.doc.blocks.remove(loc.block);
            ed.selection = Selection::caret(Pos::new(loc.block.saturating_sub(1), 0));
        });
    }

    /// Moves to the next cell (Tab). In the last cell, adds a row first.
    pub fn table_next_cell(&mut self) {
        let loc = self.selection.head.loc;
        let Some(cell) = loc.cell else { return };
        let Some(table) = self.doc.block(loc.block).and_then(Block::as_table) else {
            return;
        };
        let next = if cell.col + 1 < table.col_count() {
            CellRef {
                row: cell.row,
                col: cell.col + 1,
            }
        } else {
            CellRef {
                row: cell.row + 1,
                col: 0,
            }
        };
        if next.row >= table.row_count() {
            self.table_insert_row_below();
        }
        let target = Loc {
            block: loc.block,
            cell: Some(next),
        };
        self.set_selection(Selection {
            anchor: Pos {
                loc: target,
                offset: 0,
            },
            head: self.end_of(target),
        });
    }

    /// Moves to the previous cell (Shift+Tab).
    pub fn table_prev_cell(&mut self) {
        let loc = self.selection.head.loc;
        let Some(cell) = loc.cell else { return };
        let Some(table) = self.doc.block(loc.block).and_then(Block::as_table) else {
            return;
        };
        let prev = if cell.col > 0 {
            CellRef {
                row: cell.row,
                col: cell.col - 1,
            }
        } else if cell.row > 0 {
            CellRef {
                row: cell.row - 1,
                col: table.col_count() - 1,
            }
        } else {
            return;
        };
        let target = Loc {
            block: loc.block,
            cell: Some(prev),
        };
        self.set_selection(Selection {
            anchor: Pos {
                loc: target,
                offset: 0,
            },
            head: self.end_of(target),
        });
    }

    fn table_edit(&mut self, f: impl FnOnce(&mut Table, CellRef) -> Option<CellRef>) {
        let loc = self.selection.head.loc;
        let Some(cell) = loc.cell else { return };
        self.change(Grouping::Separate, |ed| {
            if let Some(Block::Table(table)) = ed.doc.block_mut(loc.block)
                && let Some(new_cell) = f(table, cell)
            {
                ed.selection = Selection::caret(Pos {
                    loc: Loc {
                        block: loc.block,
                        cell: Some(new_cell),
                    },
                    offset: 0,
                });
            }
        });
    }

    // ----- Clipboard ---------------------------------------------------------------------------

    /// Copies the selection.
    pub fn copy(&self) -> Fragment {
        let (start, end) = self.selection.ordered();
        if start.loc == end.loc {
            let text = self
                .doc
                .text(start.loc)
                .map(|t| t.slice(start.offset..end.offset))
                .unwrap_or_default();
            return Fragment {
                blocks: vec![Block::Text(TextBlock::paragraph(text))],
                inline: true,
            };
        }
        let mut blocks = Vec::new();
        for i in start.loc.block..=end.loc.block {
            let Some(block) = self.doc.block(i) else {
                continue;
            };
            let block = match block {
                Block::Text(t) => {
                    let from = if i == start.loc.block {
                        start.offset
                    } else {
                        0
                    };
                    let to = if i == end.loc.block {
                        end.offset
                    } else {
                        t.text.len()
                    };
                    Block::Text(t.with_text(t.text.slice(from..to)))
                }
                other => other.clone(),
            };
            blocks.push(block);
        }
        Fragment {
            blocks,
            inline: false,
        }
    }

    /// Copies and then deletes the selection.
    pub fn cut(&mut self) -> Fragment {
        let fragment = self.copy();
        if !self.selection.is_collapsed() {
            self.change(Grouping::Separate, |ed| {
                ed.delete_selection_inner();
            });
        }
        fragment
    }

    /// Pastes a fragment over the selection.
    pub fn paste(&mut self, fragment: &Fragment) {
        self.change(Grouping::Separate, |ed| {
            let caret = ed.delete_selection_inner();
            if caret.loc.cell.is_some() || ed.is_code(caret.loc) {
                let marks = ed.insert_marks(caret);
                ed.insert_at(caret, &fragment.plain_text(), marks);
            } else {
                ed.selection = Selection::caret(ed.insert_fragment_inner(caret, fragment.clone()));
            }
        });
    }

    // ----- Undo --------------------------------------------------------------------------------

    /// Replaces the whole document (e.g. restoring an earlier version) as one undoable step.
    pub fn replace_document(&mut self, doc: Document) {
        self.change(Grouping::Separate, |ed| {
            ed.doc.blocks = doc.blocks;
            ed.selection = Selection::caret(ed.doc.start());
        });
    }

    /// Undoes the last change.
    pub fn undo(&mut self) {
        if let Some(selection) = self.history.undo(&mut self.doc.blocks) {
            self.after_history(selection);
        }
    }

    /// Redoes the last undone change.
    pub fn redo(&mut self) {
        if let Some(selection) = self.history.redo(&mut self.doc.blocks) {
            self.after_history(selection);
        }
    }

    fn after_history(&mut self, selection: Selection) {
        self.version += 1;
        self.extra.clear();
        self.stored_marks = None;
        self.selection = Selection {
            anchor: self.doc.clamp(selection.anchor),
            head: self.doc.clamp(selection.head),
        };
    }

    // ----- Internals ---------------------------------------------------------------------------

    /// Runs an edit, restores the document invariants and records it for undo.
    pub(crate) fn change(&mut self, grouping: Grouping, edit: impl FnOnce(&mut Self)) {
        let before = self.doc.blocks.clone();
        let selection_before = self.selection;
        edit(self);
        self.doc.normalize();
        self.selection = Selection {
            anchor: self.doc.clamp(self.selection.anchor),
            head: self.doc.clamp(self.selection.head),
        };
        if !std::mem::take(&mut self.keep_extra) {
            self.extra.clear();
        }
        let diff = diff_blocks(&before, &self.doc.blocks);
        if !diff.is_empty() {
            self.history
                .record(&before, diff, selection_before, self.selection, grouping);
            self.version += 1;
        }
        debug_assert!(self.doc.is_valid(), "edit broke document invariants");
    }

    fn insert_marks(&self, caret: Pos) -> Marks {
        self.doc
            .text(caret.loc)
            .map(|t| t.marks_for_insert(caret.offset))
            .unwrap_or_default()
    }

    /// Inserts plain text at `caret` and moves the caret after it.
    fn insert_at(&mut self, caret: Pos, text: &str, marks: Marks) {
        let marks = self.stored_marks.take().unwrap_or(marks);
        if let Some(t) = self.doc.text_mut(caret.loc) {
            t.insert(caret.offset, text, marks);
            self.selection = Selection::caret(Pos {
                offset: caret.offset + text.len(),
                ..caret
            });
        }
    }

    fn is_code(&self, loc: Loc) -> bool {
        loc.cell.is_none()
            && matches!(
                self.doc.block(loc.block),
                Some(Block::Text(TextBlock {
                    kind: TextKind::Code,
                    ..
                }))
            )
    }

    /// Deletes the selected content and returns the caret position left behind.
    fn delete_selection_inner(&mut self) -> Pos {
        let (start, end) = self.selection.ordered();
        if start == end {
            return start;
        }
        let caret = self.delete_range(start, end);
        self.selection = Selection::caret(caret);
        caret
    }

    fn delete_range(&mut self, start: Pos, end: Pos) -> Pos {
        if start.loc == end.loc {
            if let Some(t) = self.doc.text_mut(start.loc) {
                t.delete(start.offset..end.offset);
            }
            return start;
        }
        if start.loc.block == end.loc.block {
            // Two cells of one table: clear the cells between, row by row.
            for (loc, range) in self.ranges_between(start, end) {
                if let Some(t) = self.doc.text_mut(loc) {
                    t.delete(range);
                }
            }
            return start;
        }

        let first = start.loc.block;
        let last = end.loc.block;
        let head = match (self.doc.block(first), start.loc.cell) {
            (Some(Block::Text(t)), None) => Some(t.with_text(t.text.slice(0..start.offset))),
            _ => None,
        };
        let tail = match (self.doc.block(last), end.loc.cell) {
            (Some(Block::Text(t)), None) => {
                Some(t.with_text(t.text.slice(end.offset..t.text.len())))
            }
            _ => None,
        };
        let caret_offset = head.as_ref().map_or(0, |h| h.text.len());
        let merged = match (head, tail) {
            (Some(mut h), Some(t)) => {
                h.text.append(t.text);
                h
            }
            (Some(h), None) => h,
            (None, Some(t)) => t,
            (None, None) => TextBlock::empty(),
        };
        self.doc
            .blocks
            .splice(first..=last, [Arc::new(Block::Text(merged))]);
        Pos::new(first, caret_offset)
    }

    /// Inserts a fragment at `caret` (in a text block) and returns the caret after it.
    fn insert_fragment_inner(&mut self, caret: Pos, fragment: Fragment) -> Pos {
        let index = caret.loc.block;
        if fragment.inline {
            let text = fragment
                .blocks
                .into_iter()
                .find_map(|b| match b {
                    Block::Text(t) => Some(t.text),
                    _ => None,
                })
                .unwrap_or_default();
            let len = text.len();
            if let Some(t) = self.doc.text_mut(caret.loc) {
                t.insert_rich(caret.offset, &text);
            }
            return Pos {
                offset: caret.offset + len,
                ..caret
            };
        }
        let Some(Block::Text(current)) = self.doc.block(index) else {
            return caret;
        };
        let mut head = current.with_text(current.text.slice(0..caret.offset));
        let tail_text = current.text.slice(caret.offset..current.text.len());
        let tail_template = current.clone();

        let mut blocks = fragment.blocks.into_iter().peekable();
        let mut out: Vec<Block> = Vec::new();
        if let Some(Block::Text(first)) = blocks.peek() {
            head.text.append(first.text.clone());
            if caret.offset == 0 {
                head.kind = first.kind;
                head.align = first.align;
                head.indent = first.indent;
                head.size = first.size;
            }
            blocks.next();
        }
        out.push(Block::Text(head));
        out.extend(blocks);

        let caret_after;
        let several = out.len() > 1;
        match out.last_mut() {
            Some(Block::Text(last)) if several => {
                caret_after = last.text.len();
                last.text.append(tail_text);
            }
            _ => {
                caret_after = 0;
                out.push(Block::Text(tail_template.with_text(tail_text)));
            }
        }
        let inserted = out.len();
        self.doc
            .blocks
            .splice(index..=index, out.into_iter().map(Arc::new));
        Pos::new(index + inserted - 1, caret_after)
    }

    /// The selected text, as (location, byte range) pieces in document order.
    pub(crate) fn selected_ranges(&self) -> Vec<(Loc, Range<usize>)> {
        let (start, end) = self.selection.ordered();
        self.ranges_between(start, end)
    }

    fn ranges_between(&self, start: Pos, end: Pos) -> Vec<(Loc, Range<usize>)> {
        if start.loc == end.loc {
            return vec![(start.loc, start.offset..end.offset)];
        }
        let mut out = Vec::new();
        let mut loc = Some(start.loc);
        while let Some(current) = loc {
            let len = self.doc.text(current).map_or(0, RichText::len);
            let from = if current == start.loc {
                start.offset
            } else {
                0
            };
            if current == end.loc {
                out.push((current, from..end.offset));
                break;
            }
            out.push((current, from..len));
            if current.block > end.loc.block {
                break;
            }
            loc = self.next_loc(current);
        }
        out
    }

    fn end_of(&self, loc: Loc) -> Pos {
        Pos {
            loc,
            offset: self.doc.text(loc).map_or(0, RichText::len),
        }
    }

    /// The text location after `loc` in reading order (next cell, or next block).
    fn next_loc(&self, loc: Loc) -> Option<Loc> {
        if let (Some(cell), Some(Block::Table(table))) = (loc.cell, self.doc.block(loc.block)) {
            if cell.col + 1 < table.col_count() {
                return Some(Loc::cell(loc.block, cell.row, cell.col + 1));
            }
            if cell.row + 1 < table.row_count() {
                return Some(Loc::cell(loc.block, cell.row + 1, 0));
            }
        }
        (loc.block + 1..self.doc.len()).find_map(|i| match self.doc.block(i)? {
            Block::Text(_) => Some(Loc::block(i)),
            Block::Table(_) => Some(Loc::cell(i, 0, 0)),
            Block::Divider | Block::Image(_) => None,
        })
    }

    /// The text location before `loc` in reading order.
    fn prev_loc(&self, loc: Loc) -> Option<Loc> {
        if let (Some(cell), Some(Block::Table(table))) = (loc.cell, self.doc.block(loc.block)) {
            if cell.col > 0 {
                return Some(Loc::cell(loc.block, cell.row, cell.col - 1));
            }
            if cell.row > 0 {
                return Some(Loc::cell(loc.block, cell.row - 1, table.col_count() - 1));
            }
        }
        (0..loc.block).rev().find_map(|i| match self.doc.block(i)? {
            Block::Text(_) => Some(Loc::block(i)),
            Block::Table(t) => Some(Loc::cell(i, t.row_count() - 1, t.col_count() - 1)),
            Block::Divider | Block::Image(_) => None,
        })
    }
}
