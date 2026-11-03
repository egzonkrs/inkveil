//! The rich-text editor view.
//!
//! [`EditorView`] owns an [`Editor`] (document, selection, undo) and its [`DocLayout`]. It turns
//! keyboard, mouse and IME input into editor commands; [`element::EditorElement`] lays out and
//! paints the document.

pub mod actions;
mod element;
mod layout;
mod spelling;

use std::ops::Range;

use gpui::{
    App, Bounds, Context, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Global,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollHandle, Task,
    UTF16Selection, Window, div, point, prelude::*, px,
};
use inkveil_doc::{
    Align, Document, Editor, FontSize, Fragment, Loc, MarkKind, Match, Motion, Pos, Rgb, Selection,
    TextKind, Unit,
};

use self::{
    actions::{
        AddCaretAbove, AddCaretBelow, AlignCenter, AlignLeft, AlignRight, Backspace, BulletList,
        CONTEXT, CaretsAtLineEnds, Checklist, ClearFormatting, CodeBlock, CollapseCarets, Copy,
        Cut, DeleteForward, DeleteWordBackward, DeleteWordForward, DocEnd, DocStart, Down,
        DuplicateLine, EditLink, Heading1, Heading2, Heading3, Indent, InsertDivider, InsertTable,
        Left, LineEnd, LineStart, Newline, NumberedList, Outdent, PageDown, PageUp, Paragraph,
        Paste, PastePlain, Quote, Redo, Right, SelectAll, SelectDocEnd, SelectDocStart, SelectDown,
        SelectLeft, SelectLineEnd, SelectLineStart, SelectRight, SelectUp, SelectWordLeft,
        SelectWordRight, ShiftTab, SoftBreak, Tab, ToggleBold, ToggleCode, ToggleItalic,
        ToggleStrike, ToggleUnderline, Undo, Up, WordLeft, WordRight,
    },
    element::EditorElement,
    layout::{BlockContent, Decorations, DocLayout},
    spelling::Spelling,
};

/// Events from the editor to its owner.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorEvent {
    /// The document changed.
    Changed,
    /// The caret or selection moved (toolbar state may need updating).
    SelectionChanged,
    /// The user asked to add or edit a link (Ctrl+K).
    EditLinkRequested,
    /// Right click: show the context menu at `position` (window coordinates).
    ContextMenu {
        position: Point<Pixels>,
        misspelled: Option<MisspelledWord>,
    },
    /// A picture was clicked: show it large.
    ImageClicked(usize),
    /// A picture was right-clicked: its menu at `position` (window coordinates).
    ImageMenu {
        index: usize,
        position: Point<Pixels>,
    },
    /// Pictures from the clipboard (encoded bytes) to insert at the caret.
    PasteImage(Vec<u8>),
}

/// A misspelled word under the mouse, for the context menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MisspelledWord {
    pub loc: Loc,
    pub range: Range<usize>,
    pub word: String,
}

/// View options shared by every editor, stored encrypted in the vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EditorSettings {
    /// Wrap long lines at the window edge (Alt+Z toggles, like VS Code).
    pub wrap: bool,
    /// Show line numbers in a gutter.
    pub line_numbers: bool,
    /// Underline misspelled words and offer suggestions.
    pub spellcheck: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            wrap: true,
            line_numbers: true,
            spellcheck: true,
        }
    }
}

impl Global for EditorSettings {}

impl EditorSettings {
    pub fn get(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_default()
    }
}

/// What a mouse drag is selecting by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragMode {
    Character,
    /// Double-click drag: keeps the initially selected word (`start..end`).
    Word(Pos, Pos),
    /// Triple-click drag: keeps the initially selected block (`start..end`).
    Text(Pos, Pos),
    /// Shift+Alt: a box from this position to the pointer, a caret on every line.
    Column(Pos),
    /// Dragging a picture's resize handle: block index, pointer x and picture width at the
    /// start, and the text column width.
    ResizeImage {
        index: usize,
        start_x: Pixels,
        start_width: Pixels,
        column: Pixels,
    },
}

/// While selecting with the mouse, the view scrolls once the pointer is this close to its top or
/// bottom edge (without wrapping, also left or right), or past it.
const DRAG_SCROLL_MARGIN: f32 = 16.;

/// Pointer movement after pressing the button that counts as a drag, so a click near an edge
/// does not scroll.
const DRAG_THRESHOLD: f32 = 3.;

/// Scroll speed in pixels per frame for a selection drag with the pointer `distance` pixels past
/// the margin: slow near the edge, faster the further out it goes.
fn drag_scroll_speed(distance: f32) -> f32 {
    (distance.max(0.).powf(1.2) / 5.).clamp(1., 60.)
}

/// Browser-style autoscroll after clicking the mouse wheel: the view scrolls toward the pointer,
/// faster the further it is from where the wheel was clicked.
struct WheelScroll {
    /// Where the wheel was clicked, in window coordinates.
    anchor: Point<Pixels>,
    pointer: Point<Pixels>,
    /// The pointer moved while the wheel was still held: releasing it stops scrolling.
    moved_while_held: bool,
    _ticker: Task<()>,
}

/// Pointer distance from the anchor that does not scroll yet.
const WHEEL_SCROLL_DEAD_ZONE: f32 = 12.;

/// Scroll speed in pixels per frame for a pointer `distance` pixels from the anchor (signed).
fn wheel_scroll_speed(distance: f32) -> f32 {
    let beyond = (distance.abs() - WHEEL_SCROLL_DEAD_ZONE).max(0.);
    distance.signum() * (beyond * 0.12 + (beyond / 100.).powi(2) * 6.)
}

/// The last rich copy, so pasting it back keeps formatting. Cleared when the vault locks; the
/// plain text is wiped from memory when dropped.
#[derive(Default)]
pub struct RichClipboard {
    text: String,
    fragment: Option<Fragment>,
}

impl Drop for RichClipboard {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.text);
    }
}

impl Global for RichClipboard {}

impl RichClipboard {
    pub fn clear(cx: &mut App) {
        cx.set_global(Self::default());
    }
}

pub struct EditorView {
    editor: Editor,
    focus_handle: FocusHandle,
    scroll: ScrollHandle,
    layout: DocLayout,
    /// Bounds of the document element in window coordinates, from the last paint.
    bounds: Option<Bounds<Pixels>>,
    /// Scroll offset at the time `bounds` was recorded, so hit-testing stays exact while scrolling.
    bounds_scroll_offset: Point<Pixels>,
    drag: Option<DragMode>,
    /// Where a selection drag started, until the pointer has moved away from it.
    drag_origin: Option<Point<Pixels>>,
    /// The pointer during a selection drag, in window coordinates (it may be outside the editor).
    drag_pointer: Option<Point<Pixels>>,
    /// Scroll offset when the drag selection was last placed under the pointer.
    drag_hit_offset: Point<Pixels>,
    /// Scrolls at the edges and keeps the selection under the pointer, once per frame while a
    /// selection drag lasts (dropping it stops it).
    drag_ticker: Option<Task<()>>,
    /// Remembered x for repeated Up/Down presses.
    goal_x: Option<Pixels>,
    /// IME composition in progress.
    marked: Option<(Loc, Range<usize>)>,
    /// Scroll the caret into view on the next paint.
    autoscroll: bool,
    /// Viewing only (history previews): the caret and selection work, edits are ignored.
    read_only: bool,
    /// Dragging the horizontal scrollbar: mouse x and scroll offset x when the drag started.
    scrollbar_drag: Option<(Pixels, Pixels)>,
    /// Dragging the vertical scrollbar: mouse y and scroll offset y when the drag started.
    vscroll_drag: Option<(Pixels, Pixels)>,
    /// A smooth scroll to the top or bottom in progress (dropping it stops it).
    scroll_animation: Option<Task<()>>,
    /// Autoscroll after a mouse wheel click, while active.
    wheel_scroll: Option<WheelScroll>,
    spelling: Spelling,
    /// Find results to highlight, in reading order, and the one the find bar is on.
    find_matches: Vec<Match>,
    find_current: Option<usize>,
    /// The picture under the pointer, which shows its outline and resize handle.
    hovered_image: Option<usize>,
}

impl EventEmitter<EditorEvent> for EditorView {}

impl EditorView {
    pub fn new(doc: Document, cx: &mut Context<Self>) -> Self {
        Self {
            editor: Editor::new(doc),
            focus_handle: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            layout: DocLayout::empty(),
            bounds: None,
            bounds_scroll_offset: point(px(0.), px(0.)),
            drag: None,
            drag_origin: None,
            drag_pointer: None,
            drag_hit_offset: point(px(0.), px(0.)),
            drag_ticker: None,
            goal_x: None,
            marked: None,
            autoscroll: false,
            read_only: false,
            scrollbar_drag: None,
            vscroll_drag: None,
            scroll_animation: None,
            wheel_scroll: None,
            spelling: Spelling::default(),
            find_matches: Vec::new(),
            find_current: None,
            hovered_image: None,
        }
    }

    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    pub fn doc(&self) -> &Document {
        self.editor.doc()
    }

    fn decorations(&self, spellcheck: bool) -> Decorations {
        Decorations {
            spelling: if spellcheck {
                self.spelling.ranges(self.editor.selection().head)
            } else {
                Vec::new()
            },
            marked: self.marked.clone(),
        }
    }

    /// Highlights find results; `current` is the one shown as selected.
    pub fn set_find_highlights(
        &mut self,
        matches: Vec<Match>,
        current: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        if matches != self.find_matches || current != self.find_current {
            self.find_matches = matches;
            self.find_current = current;
            cx.notify();
        }
    }

    /// Find results at `loc` as (byte range, is current) pairs.
    fn find_highlights_at(&self, loc: Loc) -> impl Iterator<Item = (Range<usize>, bool)> + '_ {
        let first = self.find_matches.partition_point(|m| m.loc < loc);
        self.find_matches[first..]
            .iter()
            .enumerate()
            .take_while(move |(_, m)| m.loc == loc)
            .map(move |(i, m)| (m.range.clone(), self.find_current == Some(first + i)))
    }

    /// Selects a range and scrolls it into view (find results).
    pub fn select_range(&mut self, loc: Loc, range: Range<usize>, cx: &mut Context<Self>) {
        self.run_horizontal(cx, |ed| {
            ed.set_selection(Selection {
                anchor: Pos {
                    loc,
                    offset: range.start,
                },
                head: Pos {
                    loc,
                    offset: range.end,
                },
            });
        });
    }

    /// Replaces `range` at `loc` with `text`, e.g. a spelling suggestion.
    pub fn replace_range(
        &mut self,
        loc: Loc,
        range: Range<usize>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        self.run_horizontal(cx, |ed| {
            ed.set_selection(Selection {
                anchor: Pos {
                    loc,
                    offset: range.start,
                },
                head: Pos {
                    loc,
                    offset: range.end,
                },
            });
            ed.insert_text(text);
        });
    }

    pub fn copy_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(&Copy, window, cx);
    }

    pub fn cut_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cut(&Cut, window, cx);
    }

    pub fn paste_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.paste(&Paste, window, cx);
    }

    pub fn select_everything(&mut self, cx: &mut Context<Self>) {
        self.run_horizontal(cx, Editor::select_all);
    }

    pub fn table_command(&mut self, command: fn(&mut Editor), cx: &mut Context<Self>) {
        self.run_horizontal(cx, command);
    }

    /// Re-checks spelling and redraws (after the dictionary loads or changes).
    pub fn refresh_spelling(&mut self, cx: &mut Context<Self>) {
        if EditorSettings::get(cx).spellcheck && self.spelling.update(self.editor.doc()) {
            cx.notify();
        }
    }

    /// A view that shows `doc` but ignores edits.
    pub fn read_only(doc: Document, cx: &mut Context<Self>) -> Self {
        Self {
            read_only: true,
            ..Self::new(doc, cx)
        }
    }

    /// Runs a command, then tells listeners what changed and scrolls the caret into view.
    pub fn run(&mut self, cx: &mut Context<Self>, command: impl FnOnce(&mut Editor)) {
        self.run_inner(true, cx, command);
    }

    /// Runs a selection command from a mouse click or drag without forcing a discrete caret jump.
    fn run_without_autoscroll(
        &mut self,
        cx: &mut Context<Self>,
        command: impl FnOnce(&mut Editor),
    ) {
        self.run_inner(false, cx, command);
    }

    fn run_inner(
        &mut self,
        autoscroll: bool,
        cx: &mut Context<Self>,
        command: impl FnOnce(&mut Editor),
    ) {
        let version = self.editor.version();
        let selection = self.editor.selection();
        if self.read_only {
            // Blocks are shared, so this copy is cheap; it lets selection moves through while
            // any change to the document is dropped.
            let before = self.editor.clone();
            command(&mut self.editor);
            if self.editor.version() != version {
                let selection = self.editor.selection();
                self.editor = before;
                self.editor.set_selection(selection);
            }
            if self.editor.selection() != selection {
                cx.emit(EditorEvent::SelectionChanged);
            }
            cx.notify();
            return;
        }
        command(&mut self.editor);
        if self.editor.version() != version {
            cx.emit(EditorEvent::Changed);
        }
        if self.editor.selection() != selection {
            cx.emit(EditorEvent::SelectionChanged);
        }
        if autoscroll {
            self.autoscroll = true;
        }
        cx.notify();
    }

    fn run_horizontal(&mut self, cx: &mut Context<Self>, command: impl FnOnce(&mut Editor)) {
        self.goal_x = None;
        self.run(cx, command);
    }

    /// Puts the caret at `pos`, e.g. when restoring a session.
    pub fn set_caret(&mut self, pos: Pos, cx: &mut Context<Self>) {
        self.run_horizontal(cx, |ed| ed.set_selection(Selection::caret(pos)));
    }

    // ----- Toolbar entry points ----------------------------------------------------------------

    pub fn toggle_mark(&mut self, kind: MarkKind, cx: &mut Context<Self>) {
        self.run(cx, |ed| ed.toggle_mark(kind));
    }

    pub fn set_text_kind(&mut self, kind: TextKind, cx: &mut Context<Self>) {
        self.run(cx, |ed| ed.set_text_kind(kind));
    }

    pub fn set_align(&mut self, align: Align, cx: &mut Context<Self>) {
        self.run(cx, |ed| ed.set_align(align));
    }

    pub fn set_size(&mut self, size: FontSize, cx: &mut Context<Self>) {
        self.run(cx, |ed| ed.set_size(size));
    }

    pub fn set_color(&mut self, color: Option<Rgb>, cx: &mut Context<Self>) {
        self.run(cx, |ed| ed.set_color(color));
    }

    pub fn set_highlight(&mut self, color: Option<Rgb>, cx: &mut Context<Self>) {
        self.run(cx, |ed| ed.set_highlight(color));
    }

    pub fn indent(&mut self, cx: &mut Context<Self>) {
        self.run(cx, Editor::indent);
    }

    pub fn outdent(&mut self, cx: &mut Context<Self>) {
        self.run(cx, Editor::outdent);
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        self.run_horizontal(cx, Editor::undo);
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        self.run_horizontal(cx, Editor::redo);
    }

    pub fn insert_divider(&mut self, cx: &mut Context<Self>) {
        self.run_horizontal(cx, Editor::insert_divider);
    }

    pub fn insert_table(&mut self, rows: u16, cols: u16, cx: &mut Context<Self>) {
        self.run_horizontal(cx, |ed| ed.insert_table(rows, cols));
    }

    pub fn clear_formatting(&mut self, cx: &mut Context<Self>) {
        self.run(cx, Editor::clear_formatting);
    }

    /// The link under the caret or selection start, if any.
    pub fn current_link(&self) -> Option<String> {
        self.editor
            .caret_marks()
            .link
            .map(|l| l.to_string())
            .or_else(|| {
                let (start, _) = self.editor.selection().ordered();
                let text = self.doc().text(start.loc)?;
                text.run_ranges()
                    .find(|(r, _)| r.start <= start.offset && start.offset < r.end)
                    .and_then(|(_, m)| m.link.as_ref().map(ToString::to_string))
            })
    }

    /// Links the selection to `url`, or inserts `url` as linked text when nothing is selected.
    /// An empty `url` removes links from the selection.
    pub fn apply_link(&mut self, url: &str, cx: &mut Context<Self>) {
        let url = url.trim();
        if url.is_empty() {
            self.run(cx, |ed| ed.set_link(None));
        } else if self.editor.selection().is_collapsed() {
            self.run(cx, |ed| ed.insert_link(url, url));
        } else {
            self.run(cx, |ed| ed.set_link(Some(url)));
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus_handle, cx);
    }

    // ----- Keyboard ------------------------------------------------------------------------------

    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        self.run_horizontal(cx, Editor::split_block);
    }

    fn soft_break(&mut self, _: &SoftBreak, _: &mut Window, cx: &mut Context<Self>) {
        self.run_horizontal(cx, Editor::insert_soft_break);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.run_horizontal(cx, |ed| ed.delete_backward(Unit::Grapheme));
    }

    fn delete_forward(&mut self, _: &DeleteForward, _: &mut Window, cx: &mut Context<Self>) {
        self.run_horizontal(cx, |ed| ed.delete_forward(Unit::Grapheme));
    }

    fn delete_word_backward(
        &mut self,
        _: &DeleteWordBackward,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_horizontal(cx, |ed| ed.delete_backward(Unit::Word));
    }

    fn delete_word_forward(
        &mut self,
        _: &DeleteWordForward,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_horizontal(cx, |ed| ed.delete_forward(Unit::Word));
    }

    fn tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        if self.editor.in_table() {
            self.run_horizontal(cx, Editor::table_next_cell);
        } else if self
            .editor
            .current_text_block()
            .is_some_and(|b| b.kind.is_list())
            || !self.editor.selection().is_within_one_text()
        {
            self.run(cx, Editor::indent);
        } else {
            self.run_horizontal(cx, |ed| ed.insert_text("\t"));
        }
    }

    fn shift_tab(&mut self, _: &ShiftTab, _: &mut Window, cx: &mut Context<Self>) {
        if self.editor.in_table() {
            self.run_horizontal(cx, Editor::table_prev_cell);
        } else {
            self.run(cx, Editor::outdent);
        }
    }

    fn motion(&mut self, motion: Motion, extend: bool, cx: &mut Context<Self>) {
        self.run_horizontal(cx, |ed| ed.move_caret(motion, extend));
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::Left, false, cx);
    }
    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::Right, false, cx);
    }
    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::WordLeft, false, cx);
    }
    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::WordRight, false, cx);
    }
    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::DocStart, false, cx);
    }
    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::DocEnd, false, cx);
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::Left, true, cx);
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::Right, true, cx);
    }
    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::WordLeft, true, cx);
    }
    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::WordRight, true, cx);
    }
    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::DocStart, true, cx);
    }
    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::DocEnd, true, cx);
    }
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.run_horizontal(cx, Editor::select_all);
    }

    /// Home/End target for `head`: the start or end of its visual row.
    fn row_edge(&self, head: Pos, end: bool) -> Option<Pos> {
        self.layout
            .blocks
            .get(head.loc.block)
            .and_then(|b| b.text_at(head.loc.cell))
            .map(|(text, _)| {
                let (start, stop) = text.row_bounds(head.offset);
                Pos {
                    offset: if end { stop } else { start },
                    ..head
                }
            })
    }

    fn line_edge(&mut self, end: bool, extend: bool, cx: &mut Context<Self>) {
        if self.editor.has_multiple_carets() {
            let moved: Vec<Selection> = self
                .editor
                .selections()
                .into_iter()
                .map(|s| {
                    let target = self.row_edge(s.head, end).unwrap_or(s.head);
                    Selection {
                        anchor: if extend { s.anchor } else { target },
                        head: target,
                    }
                })
                .collect();
            return self.run_horizontal(cx, |ed| ed.set_selections(moved));
        }
        let head = self.editor.selection().head;
        let target = self
            .layout
            .blocks
            .get(head.loc.block)
            .and_then(|b| b.text_at(head.loc.cell))
            .map(|(text, _)| {
                let (start, stop) = text.row_bounds(head.offset);
                Pos {
                    offset: if end { stop } else { start },
                    ..head
                }
            });
        match target {
            Some(target) => self.run_horizontal(cx, |ed| {
                let anchor = if extend {
                    ed.selection().anchor
                } else {
                    target
                };
                ed.set_selection(Selection {
                    anchor,
                    head: target,
                });
            }),
            None => self.motion(
                if end {
                    Motion::BlockEnd
                } else {
                    Motion::BlockStart
                },
                extend,
                cx,
            ),
        }
    }

    fn line_start(&mut self, _: &LineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.line_edge(false, false, cx);
    }
    fn line_end(&mut self, _: &LineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.line_edge(true, false, cx);
    }
    fn select_line_start(&mut self, _: &SelectLineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.line_edge(false, true, cx);
    }
    fn select_line_end(&mut self, _: &SelectLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.line_edge(true, true, cx);
    }

    /// Moves the caret `rows` visual rows up (negative) or down, keeping the column.
    /// The position `rows` visual rows above (negative) or below `head`, at column `x`.
    fn row_target(&self, head: Pos, rows: f32, x: Option<Pixels>) -> Option<Pos> {
        let (caret, line_height) = self.layout.caret(head)?;
        let x = x.unwrap_or(caret.x);
        let y = caret.y + line_height * rows + line_height / 2.;
        Some(if y < px(0.) {
            self.doc().start()
        } else if y > self.layout.height {
            self.doc().end()
        } else {
            self.layout.hit(self.doc(), point(x, y))
        })
    }

    fn vertical(&mut self, rows: f32, extend: bool, cx: &mut Context<Self>) {
        if self.editor.has_multiple_carets() {
            let moved: Vec<Selection> = self
                .editor
                .selections()
                .into_iter()
                .map(|s| {
                    let target = self.row_target(s.head, rows, None).unwrap_or(s.head);
                    Selection {
                        anchor: if extend { s.anchor } else { target },
                        head: target,
                    }
                })
                .collect();
            return self.run(cx, |ed| ed.set_selections(moved));
        }
        let selection = self.editor.selection();
        if !extend && !selection.is_collapsed() && rows.abs() <= 1. {
            let (start, end) = selection.ordered();
            let pos = if rows < 0. { start } else { end };
            return self.run(cx, |ed| ed.set_selection(Selection::caret(pos)));
        }
        let Some((caret, line_height)) = self.layout.caret(selection.head) else {
            return;
        };
        let x = *self.goal_x.get_or_insert(caret.x);
        // Aim at the middle of the target row.
        let y = caret.y + line_height * rows + line_height / 2.;
        let target = if y < px(0.) {
            self.doc().start()
        } else if y > self.layout.height {
            self.doc().end()
        } else {
            self.layout.hit(self.doc(), point(x, y))
        };
        self.run(cx, |ed| {
            let anchor = if extend {
                ed.selection().anchor
            } else {
                target
            };
            ed.set_selection(Selection {
                anchor,
                head: target,
            });
        });
    }

    fn page_rows(&self) -> f32 {
        let viewport = self.scroll.bounds().size.height;
        (viewport / px(layout::BASE_FONT_SIZE * 1.55))
            .floor()
            .max(1.)
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1., false, cx);
    }
    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1., false, cx);
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1., true, cx);
    }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1., true, cx);
    }
    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        let rows = self.page_rows();
        self.vertical(-rows, false, cx);
    }
    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        let rows = self.page_rows();
        self.vertical(rows, false, cx);
    }

    // ----- Clipboard and history ---------------------------------------------------------------

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if self.editor.has_multiple_carets() {
            // Like VS Code: every selection's text, one per line, in document order.
            let mut selections = self.editor.selections();
            selections.sort_by_key(|s| s.ordered().0);
            let text = selections
                .iter()
                .filter(|s| s.is_within_one_text())
                .filter_map(|s| {
                    let (a, b) = s.ordered();
                    self.doc().text(a.loc)?.as_str().get(a.offset..b.offset)
                })
                .collect::<Vec<_>>()
                .join("\n");
            crate::clipboard::write_text(&text, cx);
            cx.set_global(RichClipboard {
                text,
                fragment: None,
            });
            return;
        }
        if self.editor.selection().is_collapsed() {
            return;
        }
        let fragment = self.editor.copy();
        let text = fragment.plain_text();
        crate::clipboard::write_text(&text, cx);
        cx.set_global(RichClipboard {
            text,
            fragment: Some(fragment),
        });
    }

    fn cut(&mut self, action: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        let _ = action;
        self.copy(&Copy, window, cx);
        self.run_horizontal(cx, |ed| {
            ed.cut();
        });
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let item = cx.read_from_clipboard();
        // A picture (a screenshot, or an image copied in another app) is inserted as a picture.
        if !self.read_only
            && let Some(item) = &item
            && item.text().is_none()
            && let Some(image) = item.entries().iter().find_map(|e| match e {
                gpui::ClipboardEntry::Image(image) => Some(image.bytes.clone()),
                _ => None,
            })
        {
            cx.emit(EditorEvent::PasteImage(image));
            return;
        }
        let Some(text) = item.and_then(|item| item.text()) else {
            return;
        };
        let rich = cx
            .try_global::<RichClipboard>()
            .filter(|r| r.text == text)
            .and_then(|r| r.fragment.clone());
        match rich {
            Some(fragment) => self.run_horizontal(cx, |ed| ed.paste(&fragment)),
            None => self.run_horizontal(cx, |ed| ed.insert_text(&text)),
        }
    }

    fn paste_plain(&mut self, _: &PastePlain, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.run_horizontal(cx, |ed| ed.insert_text(&text));
        }
    }

    fn on_undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        self.undo(cx);
    }

    fn on_redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        self.redo(cx);
    }

    // ----- Formatting shortcuts ----------------------------------------------------------------

    fn bold(&mut self, _: &ToggleBold, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_mark(MarkKind::Bold, cx);
    }
    fn italic(&mut self, _: &ToggleItalic, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_mark(MarkKind::Italic, cx);
    }
    fn underline(&mut self, _: &ToggleUnderline, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_mark(MarkKind::Underline, cx);
    }
    fn strike(&mut self, _: &ToggleStrike, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_mark(MarkKind::Strike, cx);
    }
    fn code(&mut self, _: &ToggleCode, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_mark(MarkKind::Code, cx);
    }
    fn on_clear_formatting(&mut self, _: &ClearFormatting, _: &mut Window, cx: &mut Context<Self>) {
        self.clear_formatting(cx);
    }
    fn edit_link(&mut self, _: &EditLink, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::EditLinkRequested);
    }
    fn paragraph(&mut self, _: &Paragraph, _: &mut Window, cx: &mut Context<Self>) {
        self.set_text_kind(TextKind::Paragraph, cx);
    }
    fn heading1(&mut self, _: &Heading1, _: &mut Window, cx: &mut Context<Self>) {
        self.set_text_kind(TextKind::Heading(1), cx);
    }
    fn heading2(&mut self, _: &Heading2, _: &mut Window, cx: &mut Context<Self>) {
        self.set_text_kind(TextKind::Heading(2), cx);
    }
    fn heading3(&mut self, _: &Heading3, _: &mut Window, cx: &mut Context<Self>) {
        self.set_text_kind(TextKind::Heading(3), cx);
    }
    fn bullet_list(&mut self, _: &BulletList, _: &mut Window, cx: &mut Context<Self>) {
        self.set_text_kind(TextKind::Bullet, cx);
    }
    fn numbered_list(&mut self, _: &NumberedList, _: &mut Window, cx: &mut Context<Self>) {
        self.set_text_kind(TextKind::Numbered, cx);
    }
    fn checklist(&mut self, _: &Checklist, _: &mut Window, cx: &mut Context<Self>) {
        self.set_text_kind(TextKind::Task { checked: false }, cx);
    }
    fn quote(&mut self, _: &Quote, _: &mut Window, cx: &mut Context<Self>) {
        self.set_text_kind(TextKind::Quote, cx);
    }
    fn code_block(&mut self, _: &CodeBlock, _: &mut Window, cx: &mut Context<Self>) {
        self.set_text_kind(TextKind::Code, cx);
    }
    fn align_left(&mut self, _: &AlignLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.set_align(Align::Left, cx);
    }
    fn align_center(&mut self, _: &AlignCenter, _: &mut Window, cx: &mut Context<Self>) {
        self.set_align(Align::Center, cx);
    }
    fn align_right(&mut self, _: &AlignRight, _: &mut Window, cx: &mut Context<Self>) {
        self.set_align(Align::Right, cx);
    }
    fn on_indent(&mut self, _: &Indent, _: &mut Window, cx: &mut Context<Self>) {
        self.indent(cx);
    }
    fn on_outdent(&mut self, _: &Outdent, _: &mut Window, cx: &mut Context<Self>) {
        self.outdent(cx);
    }
    fn on_insert_divider(&mut self, _: &InsertDivider, _: &mut Window, cx: &mut Context<Self>) {
        self.insert_divider(cx);
    }
    fn on_insert_table(&mut self, _: &InsertTable, _: &mut Window, cx: &mut Context<Self>) {
        self.insert_table(3, 3, cx);
    }

    // ----- Lines and multiple carets ----------------------------------------------------------

    fn duplicate_line(&mut self, _: &DuplicateLine, _: &mut Window, cx: &mut Context<Self>) {
        self.run_horizontal(cx, Editor::duplicate_lines);
    }

    fn add_caret_vertically(&mut self, rows: f32, cx: &mut Context<Self>) {
        let head = self.editor.selection().head;
        if let Some(target) = self.row_target(head, rows, self.goal_x)
            && target != head
        {
            self.run(cx, |ed| ed.add_caret(target));
        }
    }

    fn add_caret_above(&mut self, _: &AddCaretAbove, _: &mut Window, cx: &mut Context<Self>) {
        self.add_caret_vertically(-1., cx);
    }

    fn add_caret_below(&mut self, _: &AddCaretBelow, _: &mut Window, cx: &mut Context<Self>) {
        self.add_caret_vertically(1., cx);
    }

    fn carets_at_line_ends(
        &mut self,
        _: &CaretsAtLineEnds,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_horizontal(cx, Editor::carets_at_line_ends);
    }

    fn collapse_carets(&mut self, _: &CollapseCarets, _: &mut Window, cx: &mut Context<Self>) {
        if self.wheel_scroll.is_some() {
            self.stop_wheel_scroll(cx);
        } else if self.editor.has_multiple_carets() {
            self.run(cx, Editor::collapse_to_primary);
        } else {
            cx.propagate();
        }
    }

    // ----- Mouse wheel click: autoscroll ---------------------------------------------------------

    fn middle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.wheel_scroll.is_some() {
            self.stop_wheel_scroll(cx);
            return;
        }
        let max = self.scroll.max_offset();
        if max.y <= px(1.) && max.x <= px(1.) {
            return;
        }
        self.scroll_animation = None;
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                let active = this
                    .update(cx, EditorView::wheel_scroll_step)
                    .unwrap_or(false);
                if !active {
                    break;
                }
            }
        });
        self.wheel_scroll = Some(WheelScroll {
            anchor: event.position,
            pointer: event.position,
            moved_while_held: false,
            _ticker: ticker,
        });
        cx.notify();
    }

    /// Moves the view one frame toward the pointer. Returns false once autoscroll has stopped.
    fn wheel_scroll_step(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(wheel) = &self.wheel_scroll else {
            return false;
        };
        let distance = wheel.pointer - wheel.anchor;
        let max = self.scroll.max_offset();
        let mut offset = self.scroll.offset();
        let before = offset;
        offset.y = (offset.y - px(wheel_scroll_speed(distance.y.as_f32()))).clamp(-max.y, px(0.));
        if !EditorSettings::get(cx).wrap {
            offset.x =
                (offset.x - px(wheel_scroll_speed(distance.x.as_f32()))).clamp(-max.x, px(0.));
        }
        if offset != before {
            self.scroll.set_offset(offset);
            cx.notify();
        }
        true
    }

    fn wheel_scroll_pointer(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if let Some(wheel) = &mut self.wheel_scroll {
            wheel.pointer = event.position;
            let far = (event.position - wheel.anchor)
                .x
                .abs()
                .max((event.position - wheel.anchor).y.abs())
                > px(WHEEL_SCROLL_DEAD_ZONE);
            if far && event.pressed_button == Some(MouseButton::Middle) {
                wheel.moved_while_held = true;
            }
            cx.notify();
        }
    }

    fn stop_wheel_scroll(&mut self, cx: &mut Context<Self>) {
        if self.wheel_scroll.take().is_some() {
            cx.notify();
        }
    }

    /// While autoscrolling, a layer over the editor shows the anchor, keeps the pointer as a
    /// scroll cursor and catches the click that ends it (so that click does not move the caret).
    fn render_wheel_scroll(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let wheel = self.wheel_scroll.as_ref()?;
        let colors = crate::theme::theme(cx);
        let anchor = wheel.anchor - self.scroll.bounds().origin;
        let stop = |this: &mut Self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>| {
            cx.stop_propagation();
            this.stop_wheel_scroll(cx);
        };
        Some(
            div()
                .id("wheel-scroll")
                .occlude()
                .absolute()
                .inset_0()
                .cursor(gpui::CursorStyle::ResizeUpDown)
                .on_mouse_down(MouseButton::Left, cx.listener(stop))
                .on_mouse_down(MouseButton::Right, cx.listener(stop))
                .on_mouse_down(MouseButton::Middle, cx.listener(stop))
                .on_mouse_up(
                    MouseButton::Middle,
                    cx.listener(|this, _, _, cx| {
                        if this
                            .wheel_scroll
                            .as_ref()
                            .is_some_and(|w| w.moved_while_held)
                        {
                            this.stop_wheel_scroll(cx);
                        }
                    }),
                )
                .on_scroll_wheel(cx.listener(|this, _, _, cx| this.stop_wheel_scroll(cx)))
                .child(
                    div()
                        .absolute()
                        .left(anchor.x - px(15.))
                        .top(anchor.y - px(15.))
                        .size(px(30.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.surface)
                        .shadow_md()
                        .child(
                            crate::ui::icon(crate::assets::Icon::ArrowUp, colors.icon)
                                .size(px(10.)),
                        )
                        .child(
                            crate::ui::icon(crate::assets::Icon::ArrowDown, colors.icon)
                                .size(px(10.)),
                        ),
                )
                .into_any_element(),
        )
    }

    // ----- Mouse ---------------------------------------------------------------------------------

    fn doc_point(&self, position: Point<Pixels>) -> Option<Point<Pixels>> {
        // `bounds` is from the last frame; the view may have scrolled since (drag scrolling, the
        // mouse wheel), and the document moved with it.
        self.bounds.map(|b| {
            let origin = b.origin + (self.scroll.offset() - self.bounds_scroll_offset);
            position - origin
        })
    }

    /// Whether a drag is following the pointer: selecting, resizing a picture, a scrollbar, or
    /// autoscroll after a wheel click.
    fn tracks_pointer(&self) -> bool {
        self.drag.is_some()
            || self.scrollbar_drag.is_some()
            || self.vscroll_drag.is_some()
            || self.wheel_scroll.is_some()
    }

    fn start_selection_drag(
        &mut self,
        mode: DragMode,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.drag = Some(mode);
        self.drag_origin = Some(position);
        self.drag_pointer = Some(position);
        self.drag_hit_offset = self.scroll.offset();
        self.scroll_animation = None;
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                let active = this
                    .update(cx, EditorView::drag_scroll_step)
                    .unwrap_or(false);
                if !active {
                    break;
                }
            }
        });
        self.drag_ticker = Some(ticker);
    }

    fn stop_drag(&mut self) {
        self.drag = None;
        self.drag_origin = None;
        self.drag_pointer = None;
        self.drag_ticker = None;
    }

    /// One frame of a selection drag: scrolls while the pointer is at or past an edge of the
    /// view, even when it is held still, and keeps the selection under the pointer whenever the
    /// view has moved (by this, or the mouse wheel). Returns false once the drag is over.
    fn drag_scroll_step(&mut self, cx: &mut Context<Self>) -> bool {
        if !matches!(
            self.drag,
            Some(
                DragMode::Character | DragMode::Word(..) | DragMode::Text(..) | DragMode::Column(_)
            )
        ) {
            return false;
        }
        let Some(pointer) = self.drag_pointer else {
            return false;
        };
        let viewport = self.scroll.bounds();
        if self.drag_origin.is_none() && viewport.size.height > px(0.) {
            let max = self.scroll.max_offset();
            let mut offset = self.scroll.offset();
            let margin_y = px(DRAG_SCROLL_MARGIN).min(viewport.size.height / 4.);
            let (top, bottom) = (viewport.top() + margin_y, viewport.bottom() - margin_y);
            if pointer.y < top {
                offset.y += px(drag_scroll_speed((top - pointer.y).as_f32()));
            } else if pointer.y > bottom {
                offset.y -= px(drag_scroll_speed((pointer.y - bottom).as_f32()));
            }
            if !EditorSettings::get(cx).wrap {
                let margin_x = px(DRAG_SCROLL_MARGIN).min(viewport.size.width / 4.);
                let (left, right) = (viewport.left() + margin_x, viewport.right() - margin_x);
                if pointer.x < left {
                    offset.x += px(drag_scroll_speed((left - pointer.x).as_f32()));
                } else if pointer.x > right {
                    offset.x -= px(drag_scroll_speed((pointer.x - right).as_f32()));
                }
            }
            offset.x = offset.x.clamp(-max.x, px(0.));
            offset.y = offset.y.clamp(-max.y, px(0.));
            if offset != self.scroll.offset() {
                self.scroll.set_offset(offset);
                cx.notify();
            }
        }
        if self.scroll.offset() != self.drag_hit_offset {
            self.update_drag_selection(pointer, cx);
        }
        true
    }

    /// Moves the end of a selection drag to the text under `pointer`. A pointer outside the view
    /// selects up to the text at the nearest edge of the view; scrolling brings in the rest.
    fn update_drag_selection(&mut self, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        self.drag_hit_offset = self.scroll.offset();
        let viewport = self.scroll.bounds();
        let pointer = if viewport.size.width > px(1.) && viewport.size.height > px(1.) {
            point(
                pointer.x.clamp(viewport.left(), viewport.right() - px(1.)),
                pointer.y.clamp(viewport.top(), viewport.bottom() - px(1.)),
            )
        } else {
            pointer
        };
        let Some(local) = self.doc_point(pointer) else {
            return;
        };
        let pos = if local.y < px(0.) {
            self.doc().start()
        } else if local.y > self.layout.height {
            self.doc().end()
        } else {
            self.layout.hit(self.doc(), local)
        };
        match self.drag {
            Some(DragMode::Column(from)) if self.editor.selection().head != pos => {
                self.run_without_autoscroll(cx, |ed| ed.column_select(from, pos));
            }
            // After a double or triple click, the first word or paragraph stays selected.
            Some(DragMode::Word(start, end) | DragMode::Text(start, end)) => {
                let (anchor, head) = if pos < start {
                    (end, pos)
                } else if pos > end {
                    (start, pos)
                } else {
                    (start, end)
                };
                let next = Selection { anchor, head };
                if self.editor.selection() != next {
                    self.run_without_autoscroll(cx, |ed| ed.set_selection(next));
                }
            }
            Some(DragMode::Character) if self.editor.selection().head != pos => {
                self.run_without_autoscroll(cx, |ed| {
                    let anchor = ed.selection().anchor;
                    ed.set_selection(Selection { anchor, head: pos });
                });
            }
            _ => {}
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        let Some(local) = self.doc_point(event.position) else {
            return;
        };

        // Pictures: the bottom-right corner resizes, anywhere else opens the picture large.
        if let Some((index, bounds)) = self.layout.image_at(local) {
            let corner = bounds.bottom_right();
            let on_handle = local.x >= corner.x - px(20.) && local.y >= corner.y - px(20.);
            if on_handle && !self.read_only {
                if let Some(&BlockContent::Image { column, .. }) =
                    self.layout.blocks.get(index).map(|b| &b.content)
                {
                    self.stop_drag();
                    self.drag = Some(DragMode::ResizeImage {
                        index,
                        start_x: local.x,
                        start_width: bounds.size.width,
                        column,
                    });
                }
            } else {
                cx.emit(EditorEvent::ImageClicked(index));
            }
            return;
        }
        let pos = self.layout.hit(self.doc(), local);

        // Clicking a checklist box ticks it.
        if pos.loc.cell.is_none()
            && let Some(block) = self.layout.blocks.get(pos.loc.block)
            && let BlockContent::Text {
                marker: layout::Marker::Checkbox(_),
                ..
            } = &block.content
            && local.x >= block.left
            && local.x < block.left + px(24.)
            && local.y >= block.top
            && local.y < block.top + block.height
        {
            self.run_without_autoscroll(cx, |ed| ed.toggle_task(pos.loc.block));
            return;
        }

        // Ctrl+click follows a link in the default browser.
        if event.modifiers.control {
            let link = self.doc().text(pos.loc).and_then(|t| {
                t.run_ranges()
                    .find(|(r, _)| r.start <= pos.offset && pos.offset < r.end)
                    .and_then(|(_, m)| m.link.clone())
            });
            if let Some(url) = link {
                if url.starts_with("http://")
                    || url.starts_with("https://")
                    || url.starts_with("mailto:")
                {
                    cx.open_url(&url);
                }
                return;
            }
        }

        // Shift+Alt+click or drag: column selection from the caret, a caret on every line between
        // (like VS Code).
        if event.modifiers.shift && event.modifiers.alt {
            let from = self.editor.selection().head;
            self.goal_x = None;
            self.run_without_autoscroll(cx, |ed| ed.column_select(from, pos));
            self.start_selection_drag(DragMode::Column(from), event.position, cx);
            return;
        }

        // Ctrl+click (away from links) or Alt+click adds a caret, or removes one already there.
        if (event.modifiers.control || event.modifiers.alt) && event.click_count == 1 {
            self.stop_drag();
            self.goal_x = None;
            self.run_without_autoscroll(cx, |ed| ed.toggle_caret(pos));
            return;
        }

        self.goal_x = None;
        match event.click_count {
            1 if event.modifiers.shift => {
                self.run_without_autoscroll(cx, |ed| {
                    let anchor = ed.selection().anchor;
                    ed.set_selection(Selection { anchor, head: pos });
                });
                self.start_selection_drag(DragMode::Character, event.position, cx);
            }
            1 => {
                self.run_without_autoscroll(cx, |ed| ed.set_selection(Selection::caret(pos)));
                self.start_selection_drag(DragMode::Character, event.position, cx);
            }
            2 => {
                self.run_without_autoscroll(cx, |ed| ed.select_word_at(pos));
                let (start, end) = self.editor.selection().ordered();
                self.start_selection_drag(DragMode::Word(start, end), event.position, cx);
            }
            _ => {
                self.run_without_autoscroll(cx, |ed| ed.select_text_at(pos));
                let (start, end) = self.editor.selection().ordered();
                self.start_selection_drag(DragMode::Text(start, end), event.position, cx);
            }
        }
    }

    fn right_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let Some(local) = self.doc_point(event.position) else {
            return;
        };
        if let Some((index, _)) = self.layout.image_at(local) {
            cx.emit(EditorEvent::ImageMenu {
                index,
                position: event.position,
            });
            return;
        }
        let pos = self.layout.hit(self.doc(), local);
        let (start, end) = self.editor.selection().ordered();
        if pos < start || pos > end || start == end {
            self.goal_x = None;
            self.run_without_autoscroll(cx, |ed| ed.set_selection(Selection::caret(pos)));
        }
        let spellcheck = EditorSettings::get(cx).spellcheck;
        let misspelled = self
            .spelling
            .ranges(Pos {
                loc: pos.loc,
                offset: usize::MAX,
            })
            .into_iter()
            .find(|(loc, range)| {
                *loc == pos.loc && range.start <= pos.offset && pos.offset <= range.end
            })
            .and_then(|(loc, range)| {
                let word = self
                    .doc()
                    .text(loc)?
                    .as_str()
                    .get(range.clone())?
                    .to_owned();
                Some(MisspelledWord { loc, range, word })
            })
            .filter(|_| spellcheck);
        cx.emit(EditorEvent::ContextMenu {
            position: event.position,
            misspelled,
        });
    }

    /// The pointer moved over the editor: shows a picture's outline and resize handle.
    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        // Drags get every pointer move from `pointer_move` instead, over the editor or not.
        if self.tracks_pointer() {
            return;
        }
        let Some(local) = self.doc_point(event.position) else {
            return;
        };
        let hovered = self.layout.image_at(local).map(|(i, _)| i);
        if hovered != self.hovered_image {
            self.hovered_image = hovered;
            cx.notify();
        }
    }

    /// Every pointer move in the window, from a listener the document element installs when it
    /// paints. The window captures the pointer while a button is held, so a drag keeps following
    /// it after it leaves the editor, or the window.
    fn pointer_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.scrollbar_drag.is_some() {
            return self.scrollbar_drag_move(event, window, cx);
        }
        if self.vscroll_drag.is_some() {
            return self.vscroll_drag_move(event, cx);
        }
        if self.wheel_scroll.is_some() {
            return self.wheel_scroll_pointer(event, cx);
        }
        let Some(drag) = self.drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            // The button went up somewhere no mouse-up event reached the editor.
            self.stop_drag();
            return;
        }
        if let DragMode::ResizeImage {
            index,
            start_x,
            start_width,
            column,
        } = drag
        {
            let Some(local) = self.doc_point(event.position) else {
                return;
            };
            let width = (start_width + (local.x - start_x)).max(px(24.));
            // Clamped to 10–100 first, so the conversion cannot truncate or lose a sign.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let percent = (width / column * 100.).round().clamp(10., 100.) as u8;
            let align = self
                .doc()
                .block(index)
                .and_then(inkveil_doc::Block::as_image)
                .map_or(inkveil_doc::Align::Left, |i| i.align);
            self.run_without_autoscroll(cx, |ed| ed.set_image_layout(index, percent, align));
            return;
        }
        if let Some(origin) = self.drag_origin {
            let moved = event.position - origin;
            if moved.x.abs().max(moved.y.abs()) > px(DRAG_THRESHOLD) {
                self.drag_origin = None;
            }
        }
        self.drag_pointer = Some(event.position);
        self.update_drag_selection(event.position, cx);
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.stop_drag();
        self.scrollbar_drag = None;
        self.vscroll_drag = None;
    }

    /// Vertical scrollbar geometry when the note is taller than the view:
    /// (view height, how far it can scroll, thumb y, thumb height), relative to the view.
    fn vertical_scrollbar(&self) -> Option<(Pixels, Pixels, Pixels, Pixels)> {
        let view = self.scroll.bounds().size.height;
        let max = self.scroll.max_offset().y;
        if view <= px(0.) || max <= px(1.) {
            return None;
        }
        let thumb = (view * (view / (view + max))).max(px(32.)).min(view);
        let progress = (-self.scroll.offset().y / max).clamp(0., 1.);
        Some((view, max, (view - thumb) * progress, thumb))
    }

    fn vscrollbar_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.scroll_animation = None;
        let Some((view, max, thumb_y, thumb)) = self.vertical_scrollbar() else {
            return;
        };
        let y = event.position.y - self.scroll.bounds().top();
        if y < thumb_y || y > thumb_y + thumb {
            // Clicking the track jumps there, centering the thumb on the pointer.
            let progress = ((y - thumb / 2.) / (view - thumb).max(px(1.))).clamp(0., 1.);
            let mut offset = self.scroll.offset();
            offset.y = -max * progress;
            self.scroll.set_offset(offset);
        }
        self.vscroll_drag = Some((event.position.y, self.scroll.offset().y));
        cx.notify();
    }

    fn vscroll_drag_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((start_y, start_offset)) = self.vscroll_drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.vscroll_drag = None;
            return;
        }
        let Some((view, max, _, thumb)) = self.vertical_scrollbar() else {
            return;
        };
        let moved = event.position.y - start_y;
        let per_pixel = max / (view - thumb).max(px(1.));
        let mut offset = self.scroll.offset();
        offset.y = (start_offset - moved * per_pixel).clamp(-max, px(0.));
        self.scroll.set_offset(offset);
        cx.notify();
    }

    /// Scrolls smoothly to the top (`to_end` false) or the bottom of the note. Only the view
    /// moves; the note and the caret are not touched.
    fn smooth_scroll(&mut self, to_end: bool, cx: &mut Context<Self>) {
        let start = self.scroll.offset().y;
        let target = if to_end {
            -self.scroll.max_offset().y
        } else {
            px(0.)
        };
        let task = cx.spawn(async move |this, cx| {
            const STEPS: u8 = 24;
            for step in 1..=STEPS {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(12))
                    .await;
                let t = f32::from(step) / f32::from(STEPS);
                // Ease out: fast at first, gentle at the end.
                let eased = 1. - (1. - t).powi(3);
                let alive = this
                    .update(cx, |this, cx| {
                        let mut offset = this.scroll.offset();
                        offset.y = start + (target - start) * eased;
                        this.scroll.set_offset(offset);
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        self.scroll_animation = Some(task);
    }

    /// Horizontal scrollbar geometry when the document is wider than the view:
    /// (view width, content width, thumb x, thumb width), all relative to the view.
    fn horizontal_scrollbar(&self) -> Option<(Pixels, Pixels, Pixels, Pixels)> {
        let view = self.scroll.bounds().size.width;
        let content = self.layout.content_width + px(element::TEXT_PADDING + 24.);
        if view <= px(0.) || content <= view + px(1.) {
            return None;
        }
        let thumb = (view * (view / content)).max(px(32.));
        let max_offset = content - view;
        let progress = (-self.scroll.offset().x / max_offset).clamp(0., 1.);
        Some((view, content, (view - thumb) * progress, thumb))
    }

    fn scrollbar_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        let Some((view, content, thumb_x, thumb)) = self.horizontal_scrollbar() else {
            return;
        };
        let x = event.position.x - self.scroll.bounds().left();
        if x < thumb_x || x > thumb_x + thumb {
            // Clicking the track jumps there, centering the thumb on the pointer.
            let progress = ((x - thumb / 2.) / (view - thumb)).clamp(0., 1.);
            let mut offset = self.scroll.offset();
            offset.x = -(content - view) * progress;
            self.scroll.set_offset(offset);
        }
        self.scrollbar_drag = Some((event.position.x, self.scroll.offset().x));
        cx.notify();
    }

    fn scrollbar_drag_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((start_x, start_offset)) = self.scrollbar_drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.scrollbar_drag = None;
            return;
        }
        let Some((view, content, _, thumb)) = self.horizontal_scrollbar() else {
            return;
        };
        let moved = event.position.x - start_x;
        let per_pixel = (content - view) / (view - thumb).max(px(1.));
        let mut offset = self.scroll.offset();
        offset.x = (start_offset - moved * per_pixel).clamp(-(content - view), px(0.));
        self.scroll.set_offset(offset);
        cx.notify();
    }

    // ----- IME helpers ---------------------------------------------------------------------------

    fn current_text(&self) -> &str {
        self.doc()
            .text(self.editor.selection().head.loc)
            .map_or("", inkveil_doc::RichText::as_str)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let text = self.current_text();
        text[..offset.min(text.len())].encode_utf16().count()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let text = self.current_text();
        let mut utf16 = 0;
        for (i, c) in text.char_indices() {
            if utf16 >= offset {
                return i;
            }
            utf16 += c.len_utf16();
        }
        text.len()
    }

    /// Selects a UTF-16 range of the caret's text, or the composition when `range` is `None`.
    fn select_ime_range(&mut self, range: Option<Range<usize>>) {
        let loc = self.editor.selection().head.loc;
        let byte_range = match range {
            Some(r) => Some(self.offset_from_utf16(r.start)..self.offset_from_utf16(r.end)),
            None => self
                .marked
                .as_ref()
                .filter(|(l, _)| *l == loc)
                .map(|(_, r)| r.clone()),
        };
        if let Some(r) = byte_range {
            self.editor.set_selection(Selection {
                anchor: Pos {
                    loc,
                    offset: r.start,
                },
                head: Pos { loc, offset: r.end },
            });
        }
    }
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let start = self.offset_from_utf16(range_utf16.start);
        let end = self.offset_from_utf16(range_utf16.end);
        actual_range.replace(self.offset_to_utf16(start)..self.offset_to_utf16(end));
        Some(self.current_text()[start..end].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let selection = self.editor.selection();
        let (anchor, head) = if selection.is_within_one_text() {
            (selection.anchor.offset, selection.head.offset)
        } else {
            (selection.head.offset, selection.head.offset)
        };
        let (a, h) = (self.offset_to_utf16(anchor), self.offset_to_utf16(head));
        Some(UTF16Selection {
            range: a.min(h)..a.max(h),
            reversed: h < a,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let loc = self.editor.selection().head.loc;
        self.marked
            .as_ref()
            .filter(|(l, _)| *l == loc)
            .map(|(_, r)| self.offset_to_utf16(r.start)..self.offset_to_utf16(r.end))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        self.select_ime_range(range_utf16);
        self.marked = None;
        if text.is_empty() {
            if !self.editor.selection().is_collapsed() {
                self.run_horizontal(cx, |ed| ed.delete_backward(Unit::Grapheme));
            }
        } else {
            self.run_horizontal(cx, |ed| ed.insert_text(text));
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        new_selected_range: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        self.select_ime_range(range_utf16);
        let (start, _) = self.editor.selection().ordered();
        if text.is_empty() {
            if !self.editor.selection().is_collapsed() {
                self.run_horizontal(cx, |ed| ed.delete_backward(Unit::Grapheme));
            }
            self.marked = None;
            return;
        }
        self.run_horizontal(cx, |ed| ed.insert_text(text));
        self.marked = Some((start.loc, start.offset..start.offset + text.len()));
        if let Some(sel) = new_selected_range {
            let marked_text = text;
            let to_bytes = |u: usize| {
                let mut count = 0;
                for (i, c) in marked_text.char_indices() {
                    if count >= u {
                        return i;
                    }
                    count += c.len_utf16();
                }
                marked_text.len()
            };
            let anchor = Pos {
                offset: start.offset + to_bytes(sel.start),
                ..start
            };
            let head = Pos {
                offset: start.offset + to_bytes(sel.end),
                ..start
            };
            self.editor.set_selection(Selection { anchor, head });
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let origin = self.bounds?.origin;
        let loc = self.editor.selection().head.loc;
        let start = Pos {
            loc,
            offset: self.offset_from_utf16(range_utf16.start),
        };
        let end = Pos {
            loc,
            offset: self.offset_from_utf16(range_utf16.end),
        };
        let (a, height) = self.layout.caret(start)?;
        let (b, _) = self.layout.caret(end)?;
        Some(Bounds::from_corners(
            origin + a,
            origin + point(b.x.max(a.x + px(1.)), b.y + height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let local = self.doc_point(point)?;
        let pos = self.layout.hit(self.doc(), local);
        (pos.loc == self.editor.selection().head.loc).then(|| self.offset_to_utf16(pos.offset))
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EditorView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if EditorSettings::get(cx).spellcheck {
            self.spelling.update(self.editor.doc());
        }
        let wrap = EditorSettings::get(cx).wrap;
        let scrollbar = if wrap {
            None
        } else {
            self.horizontal_scrollbar()
        };
        let vscrollbar = self.vertical_scrollbar();
        // Past halfway the button goes back to the top; before that it goes to the bottom.
        let jump_to_end =
            vscrollbar.is_none_or(|(_, max, _, _)| -self.scroll.offset().y < max / 2.);
        let show_jump = vscrollbar.is_some_and(|(view, max, _, _)| max > view / 3.);
        let colors = crate::theme::theme(cx);
        let scroll_view = div()
            .id("editor-scroll")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .when(wrap, gpui::StatefulInteractiveElement::overflow_y_scroll)
            // A flex row lets the content be wider than the view, so it can scroll sideways.
            // `items_start` keeps the content at its own height; stretched to the view's height
            // it would leave nothing to scroll vertically.
            .when(!wrap, |d| d.flex().items_start().overflow_scroll())
            .track_scroll(&self.scroll)
            .cursor_text()
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::soft_break))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete_forward))
            .on_action(cx.listener(Self::delete_word_backward))
            .on_action(cx.listener(Self::delete_word_forward))
            .on_action(cx.listener(Self::tab))
            .on_action(cx.listener(Self::shift_tab))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::line_start))
            .on_action(cx.listener(Self::line_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_line_start))
            .on_action(cx.listener(Self::select_line_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::paste_plain))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::bold))
            .on_action(cx.listener(Self::italic))
            .on_action(cx.listener(Self::underline))
            .on_action(cx.listener(Self::strike))
            .on_action(cx.listener(Self::code))
            .on_action(cx.listener(Self::on_clear_formatting))
            .on_action(cx.listener(Self::edit_link))
            .on_action(cx.listener(Self::paragraph))
            .on_action(cx.listener(Self::heading1))
            .on_action(cx.listener(Self::heading2))
            .on_action(cx.listener(Self::heading3))
            .on_action(cx.listener(Self::bullet_list))
            .on_action(cx.listener(Self::numbered_list))
            .on_action(cx.listener(Self::checklist))
            .on_action(cx.listener(Self::quote))
            .on_action(cx.listener(Self::code_block))
            .on_action(cx.listener(Self::align_left))
            .on_action(cx.listener(Self::align_center))
            .on_action(cx.listener(Self::align_right))
            .on_action(cx.listener(Self::on_indent))
            .on_action(cx.listener(Self::on_outdent))
            .on_action(cx.listener(Self::on_insert_divider))
            .on_action(cx.listener(Self::on_insert_table))
            .on_action(cx.listener(Self::duplicate_line))
            .on_action(cx.listener(Self::add_caret_above))
            .on_action(cx.listener(Self::add_caret_below))
            .on_action(cx.listener(Self::carets_at_line_ends))
            .on_action(cx.listener(Self::collapse_carets))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::right_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::middle_mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_scroll_wheel(cx.listener(|this, _, _, _| this.scroll_animation = None))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(
                div()
                    .when(wrap, gpui::Styled::w_full)
                    // Sized to its content (the longest line), never narrower than the view.
                    .when(!wrap, |d| d.flex_none().min_w_full())
                    .pl(px(12.))
                    .pr(px(48.))
                    .pt(px(16.))
                    .pb(px(160.))
                    .child(EditorElement::new(cx.entity())),
            );
        let vertical_shown = vscrollbar.is_some();
        let horizontal_shown = scrollbar.is_some();
        div()
            .relative()
            .size_full()
            .child(scroll_view)
            .children(self.render_wheel_scroll(cx))
            .children(vscrollbar.map(|(_, _, thumb_y, thumb)| {
                div()
                    .id("vscrollbar")
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom(if horizontal_shown { px(12.) } else { px(0.) })
                    .w(px(12.))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::vscrollbar_mouse_down))
                    .child(
                        div()
                            .absolute()
                            .left(px(3.))
                            .top(thumb_y)
                            .h(thumb)
                            .w(px(6.))
                            .rounded_full()
                            .bg(colors.muted)
                            .opacity(if self.vscroll_drag.is_some() {
                                0.9
                            } else {
                                0.45
                            })
                            .hover(|d| d.opacity(0.9)),
                    )
            }))
            .when(show_jump, |d| {
                d.child(
                    div()
                        .id(if jump_to_end {
                            "scroll-to-end"
                        } else {
                            "scroll-to-top"
                        })
                        .absolute()
                        .right(px(26.))
                        .bottom(if horizontal_shown { px(28.) } else { px(20.) })
                        .size(px(36.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.surface)
                        .shadow_md()
                        .cursor_pointer()
                        .opacity(0.85)
                        .hover(|d| d.opacity(1.).bg(colors.hover))
                        .tooltip(move |_, cx| {
                            crate::ui::Tooltip::view(
                                if jump_to_end {
                                    "Scroll to the bottom"
                                } else {
                                    "Scroll to the top"
                                },
                                cx,
                            )
                        })
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.smooth_scroll(jump_to_end, cx);
                        }))
                        .child(crate::ui::icon(
                            if jump_to_end {
                                crate::assets::Icon::ArrowDown
                            } else {
                                crate::assets::Icon::ArrowUp
                            },
                            colors.icon,
                        )),
                )
            })
            .children(scrollbar.map(|(_, _, thumb_x, thumb)| {
                div()
                    .id("hscrollbar")
                    .absolute()
                    .left_0()
                    .right(if vertical_shown { px(12.) } else { px(0.) })
                    .bottom_0()
                    .h(px(12.))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::scrollbar_mouse_down))
                    .child(
                        div()
                            .absolute()
                            .top(px(3.))
                            .left(thumb_x)
                            .w(thumb)
                            .h(px(6.))
                            .rounded_full()
                            .bg(colors.muted)
                            .opacity(if self.scrollbar_drag.is_some() {
                                0.9
                            } else {
                                0.55
                            })
                            .hover(|d| d.opacity(0.9)),
                    )
            }))
    }
}
