//! A text field: one line (with optional password masking), or several lines that wrap and grow
//! with the text.
//!
//! Based on GPUI's input example: platform IME support comes from implementing
//! [`EntityInputHandler`], so accents, dead keys and Asian input methods all work.

use std::ops::Range;

use gpui::{
    App, AvailableSpace, Bounds, ContentMask, Context, CursorStyle, DispatchPhase, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PaintQuad, Pixels, Point, ScrollWheelEvent, SharedString, Style, TextAlign, TextRun,
    UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div, fill, point, prelude::*, px,
    relative, size,
};
use smallvec::SmallVec;
use unicode_segmentation::UnicodeSegmentation;
use zeroize::Zeroizing;

use crate::theme::theme;

actions!(
    text_field,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        DocStart,
        DocEnd,
        SelectDocStart,
        SelectDocEnd,
        Paste,
        Cut,
        Copy,
        Undo,
        Redo,
        Submit,
        Newline,
        CloseMenu,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteWordRight,
        FocusNext,
        FocusPrev,
    ]
);

/// Key context for text field bindings.
pub const CONTEXT: &str = "TextField";
/// Key context of a field with several lines, on top of [`CONTEXT`].
const AREA_CONTEXT: &str = "TextArea";
/// Room kept right of the caret at the end of a line.
const CARET_ROOM: f32 = 2.;
/// How wide a selected line break is drawn.
const LINE_BREAK_WIDTH: f32 = 5.;
/// How many changes Ctrl+Z can take back.
const UNDO_STEPS: usize = 200;

/// Events a text field emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFieldEvent {
    /// The text changed.
    Changed,
    /// Enter was pressed.
    Submitted,
    /// Focus left the field.
    Blurred,
}

/// The text and selection at one moment, for undo. Wiped on drop like the text itself.
struct Snapshot {
    content: Zeroizing<String>,
    selection: Range<usize>,
}

/// A text field.
#[allow(clippy::struct_excessive_bools)]
pub struct TextField {
    focus_handle: FocusHandle,
    /// Wiped on drop, since it may hold a password.
    content: Zeroizing<String>,
    placeholder: SharedString,
    masked: bool,
    borderless: bool,
    /// For a field with several lines: how many it shows before the text scrolls.
    max_lines: Option<usize>,
    /// The most characters typing or pasting can make the text.
    max_chars: Option<usize>,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    /// The caret is at the end of a wrapped row, not at the start of the next one. Both are the
    /// same place in the text.
    caret_at_row_end: bool,
    /// The column that Up and Down keep to.
    goal_x: Option<Pixels>,
    last_layout: Option<FieldLayout>,
    last_bounds: Option<Bounds<Pixels>>,
    /// How far the text is scrolled, so the caret stays in view: sideways in a one-line field,
    /// down in a field with several lines.
    scroll: Point<Pixels>,
    /// Bring the caret into view when the field is next drawn.
    reveal_caret: bool,
    is_selecting: bool,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The last change typed a character, so the next one joins its undo step.
    typing: bool,
    blink: super::blink::Blink,
    /// The right-click menu, and where it opened (window coordinates).
    menu: Option<Point<Pixels>>,
}

impl EventEmitter<TextFieldEvent> for TextField {}

impl TextField {
    pub fn new(
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle().tab_stop(true);
        cx.on_blur(&focus_handle, window, |this, _, cx| {
            this.is_selecting = false;
            cx.emit(TextFieldEvent::Blurred);
        })
        .detach();
        Self {
            focus_handle,
            content: Zeroizing::new(String::new()),
            placeholder: placeholder.into(),
            masked: false,
            borderless: false,
            max_lines: None,
            max_chars: None,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            caret_at_row_end: false,
            goal_x: None,
            last_layout: None,
            last_bounds: None,
            scroll: Point::default(),
            reveal_caret: false,
            is_selecting: false,
            undo: Vec::new(),
            redo: Vec::new(),
            typing: false,
            blink: super::blink::Blink::new(),
            menu: None,
        }
    }

    /// Shows dots instead of the text and disables copying.
    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    /// Shows or hides the text of a masked field (the "show key" eye button).
    pub fn set_masked(&mut self, masked: bool, cx: &mut Context<Self>) {
        if self.masked != masked {
            self.masked = masked;
            cx.notify();
        }
    }

    /// Draws only the text, inheriting font and size from the parent (for titles and tags).
    pub fn borderless(mut self) -> Self {
        self.borderless = true;
        self
    }

    /// Lets the text wrap and the field grow with it, up to `max_lines` before it scrolls.
    /// Shift+Enter starts a new line; Enter still submits.
    pub fn multiline(mut self, max_lines: usize) -> Self {
        self.max_lines = Some(max_lines.max(1));
        self
    }

    /// Stops typing and pasting at `max` characters, so a whole document pasted by mistake
    /// does not become a title. Text set from code is not cut.
    pub fn max_chars(mut self, max: usize) -> Self {
        self.max_chars = Some(max);
        self
    }

    /// As much of `text` as fits when it replaces `range`.
    fn fitting<'a>(&self, text: &'a str, range: &Range<usize>) -> &'a str {
        let Some(max) = self.max_chars else {
            return text;
        };
        let kept = self.content.chars().count() - self.content[range.clone()].chars().count();
        let room = max.saturating_sub(kept);
        text.char_indices()
            .nth(room)
            .map_or(text, |(end, _)| &text[..end])
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let placeholder = placeholder.into();
        if placeholder != self.placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// Replaces the text from code. Starts a fresh undo history.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.content = Zeroizing::new(self.cleaned(text));
        self.selected_range = self.content.len()..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        self.undo.clear();
        self.redo.clear();
        self.moved();
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_text("", cx);
    }

    /// Selects everything, so that typing replaces it (a search field that gets the keyboard).
    pub fn select_everything(&mut self, cx: &mut Context<Self>) {
        self.select_range(0..self.content.len(), cx);
    }

    #[cfg(test)]
    pub fn selected_text(&self) -> &str {
        &self.content[self.selected_range.clone()]
    }

    /// Text as this field accepts it: line breaks only where the field has several lines.
    fn cleaned(&self, text: &str) -> String {
        if self.max_lines.is_some() {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.replace(['\r', '\n'], " ")
        }
    }

    /// The caret went somewhere: show it, and forget what only held for where it was.
    fn moved(&mut self) {
        self.caret_at_row_end = false;
        self.goal_x = None;
        self.reveal_caret = true;
        self.typing = false;
        self.blink.reset();
    }

    /// `offset`, made safe to use in the text whatever it was computed from.
    fn valid(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.content.len());
        while !self.content.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    /// The layout of the last frame, if it still shows this text.
    fn layout(&self) -> Option<&FieldLayout> {
        let drawn = self.content_to_display(self.content.len());
        self.last_layout
            .as_ref()
            .filter(|layout| !self.content.is_empty() && layout.len == drawn)
    }

    /// The row of text the caret is on, and whether that row ends where the text wraps.
    fn caret_row(&self) -> Option<(Range<usize>, bool)> {
        let layout = self.layout()?;
        let caret = self.content_to_display(self.cursor_offset());
        let row = layout.rows[layout.row_of(caret, self.caret_at_row_end)];
        let range = self.display_to_content(row.start)..self.display_to_content(row.end);
        Some((range, row.wrapped))
    }

    fn row_start(&self) -> usize {
        self.caret_row().map_or(0, |(row, _)| row.start)
    }

    /// Where End takes the caret, and whether that is the end of a wrapped row.
    fn row_end(&self) -> (usize, bool) {
        self.caret_row()
            .map_or((self.content.len(), false), |(row, wrapped)| {
                (row.end, wrapped)
            })
    }

    /// One row up or down from the caret, keeping its column. Past the first or last row it
    /// is the start or end of the text.
    fn vertical_target(&mut self, down: bool) -> (usize, bool) {
        let edge = if down { self.content.len() } else { 0 };
        let Some(layout) = self.layout() else {
            return (edge, false);
        };
        let caret = self.content_to_display(self.cursor_offset());
        let row = layout.row_of(caret, self.caret_at_row_end);
        let goal = self.goal_x.unwrap_or_else(|| layout.x_in_row(row, caret));
        let target = if down {
            Some(row + 1).filter(|row| *row < layout.rows.len())
        } else {
            row.checked_sub(1)
        };
        let found = target.map(|target| {
            let index = layout.index_in_row(target, goal);
            let row = layout.rows[target];
            (
                self.display_to_content(index),
                row.wrapped && index == row.end,
            )
        });
        self.goal_x = Some(goal);
        found.unwrap_or((edge, false))
    }

    fn move_vertically(&mut self, down: bool, select: bool, cx: &mut Context<Self>) {
        let (offset, at_row_end) = self.vertical_target(down);
        let goal = self.goal_x;
        if select {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
        self.caret_at_row_end = at_row_end;
        self.goal_x = goal;
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(false, false, cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(true, false, cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(false, true, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(true, true, cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word(self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(self.cursor_offset()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_word(self.cursor_offset()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_word(self.cursor_offset()), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_range(0..self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.row_start(), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let (offset, at_row_end) = self.row_end();
        self.move_to(offset, cx);
        self.caret_at_row_end = at_row_end;
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.row_start(), cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        let (offset, at_row_end) = self.row_end();
        self.select_to(offset, cx);
        self.caret_at_row_end = at_row_end;
    }

    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_next(cx);
    }

    fn focus_prev(&mut self, _: &FocusPrev, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_prev(cx);
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextFieldEvent::Submitted);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_word(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_word_right(
        &mut self,
        _: &DeleteWordRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_word(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let (index, at_row_end) = self.index_for_mouse_position(event.position);
        // Only a plain press starts a drag: after a double click, a hand that moves a little
        // must not shrink the word that was just selected.
        self.is_selecting = event.click_count <= 1;
        if event.modifiers.shift {
            self.select_to(index, cx);
            return;
        }
        match event.click_count {
            0 | 1 => {
                self.move_to(index, cx);
                self.caret_at_row_end = at_row_end;
            }
            2 => self.select_range(self.word_at(index), cx),
            _ => self.select_range(self.paragraph_at(index), cx),
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    /// Right click: the menu every text box has. A click outside the selection moves the caret
    /// there first, so Paste goes where the click was.
    fn on_right_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let (index, at_row_end) = self.index_for_mouse_position(event.position);
        if !(self.selected_range.start..=self.selected_range.end).contains(&index) {
            self.move_to(index, cx);
            self.caret_at_row_end = at_row_end;
        }
        self.menu = Some(event.position);
        cx.notify();
    }

    fn close_menu(&mut self, _: &CloseMenu, _: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    fn render_menu(&self, position: Point<Pixels>, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        // A masked field gives nothing away: no Cut or Copy.
        let can_copy = !self.masked && !self.selected_range.is_empty();
        let row = |id: &'static str, label: &'static str, keys: &'static str, enabled: bool| {
            div()
                .id(id)
                .debug_selector(|| id.to_owned())
                .flex()
                .items_center()
                .justify_between()
                .gap_6()
                .h(px(28.))
                .px_2()
                .rounded_sm()
                .text_color(if enabled { colors.text } else { colors.muted })
                .when(enabled, |d| {
                    d.cursor_pointer().hover(|d| d.bg(colors.hover))
                })
                .child(label)
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.muted)
                        .child(keys),
                )
        };
        let act = |action: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            cx.listener(move |this, _: &gpui::ClickEvent, window, cx| {
                this.menu = None;
                action(this, window, cx);
                cx.notify();
            })
        };
        let panel = div()
            .id("field-menu")
            .occlude()
            .flex()
            .flex_col()
            .p_1()
            .min_w(px(180.))
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .shadow_md()
            .text_size(px(13.))
            .font_weight(gpui::FontWeight::NORMAL)
            .line_height(px(20.))
            .cursor(CursorStyle::Arrow)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.menu = None;
                cx.notify();
            }))
            .child(
                row("field-cut", "Cut", "Ctrl+X", can_copy).when(can_copy, |d| {
                    d.on_click(act(|this, w, cx| this.cut(&Cut, w, cx)))
                }),
            )
            .child(
                row("field-copy", "Copy", "Ctrl+C", can_copy).when(can_copy, |d| {
                    d.on_click(act(|this, w, cx| this.copy(&Copy, w, cx)))
                }),
            )
            .child(
                row("field-paste", "Paste", "Ctrl+V", true)
                    .on_click(act(|this, w, cx| this.paste(&Paste, w, cx))),
            )
            .child(div().h(px(1.)).mx_1().my_1().bg(colors.border))
            .child(
                row("field-select-all", "Select all", "Ctrl+A", true)
                    .on_click(act(|this, w, cx| this.select_all(&SelectAll, w, cx))),
            );
        gpui::deferred(
            gpui::anchored()
                .position(position)
                .snap_to_window()
                .child(panel),
        )
        .with_priority(2)
    }

    /// The mouse moved anywhere in the window. A drag that began in the field goes on selecting
    /// when it leaves the field.
    fn on_drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.is_selecting {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            // The button was let go somewhere that did not tell the field.
            self.is_selecting = false;
            return;
        }
        let (index, at_row_end) = self.index_for_mouse_position(event.position);
        if index != self.cursor_offset() {
            self.select_to(index, cx);
            self.caret_at_row_end = at_row_end;
        }
    }

    /// The wheel scrolls a field whose text is taller than it.
    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(layout), Some(bounds)) = (self.last_layout.as_ref(), self.last_bounds) else {
            return;
        };
        if self.max_lines.is_none() {
            return;
        }
        let furthest = (layout.height() - bounds.size.height).max(px(0.));
        let moved = event.delta.pixel_delta(layout.line_height).y;
        let y = (self.scroll.y - moved).clamp(px(0.), furthest);
        if y != self.scroll.y {
            self.scroll.y = y;
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.masked && !self.selected_range.is_empty() {
            crate::clipboard::write_text(&self.content[self.selected_range.clone()], cx);
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        // Only what reached the clipboard is removed.
        if !self.masked
            && !self.selected_range.is_empty()
            && crate::clipboard::write_text(&self.content[self.selected_range.clone()], cx)
        {
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            content: self.content.clone(),
            selection: self.selected_range.clone(),
        }
    }

    /// Keeps the text as it is now, to come back to with Ctrl+Z. A run of typed characters is
    /// one step. A masked field keeps nothing: no extra copies of a password.
    fn remember(&mut self, typing: bool) {
        if self.masked || (typing && self.typing) {
            return;
        }
        if self.undo.len() == UNDO_STEPS {
            self.undo.remove(0);
        }
        self.undo.push(self.snapshot());
        self.redo.clear();
    }

    fn restore(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.content = snapshot.content;
        self.selected_range =
            self.valid(snapshot.selection.start)..self.valid(snapshot.selection.end);
        self.selection_reversed = false;
        self.marked_range = None;
        self.moved();
        cx.emit(TextFieldEvent::Changed);
        cx.notify();
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(snapshot) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.restore(snapshot, cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(snapshot) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.restore(snapshot, cx);
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.valid(offset);
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.moved();
        cx.notify();
    }

    fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.selected_range = self.valid(range.start)..self.valid(range.end);
        self.selection_reversed = false;
        self.moved();
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    /// The place in the text nearest to `position`, and whether it is the end of a wrapped row.
    fn index_for_mouse_position(&self, position: Point<Pixels>) -> (usize, bool) {
        let (Some(bounds), Some(layout)) = (self.last_bounds, self.layout()) else {
            return (0, false);
        };
        let local = position - bounds.origin + self.scroll;
        if local.y < px(0.) {
            return (0, false);
        }
        if local.y >= layout.height() {
            return (self.content.len(), false);
        }
        let row = layout.row_at(local.y);
        let index = layout.index_in_row(row, local.x);
        let row = layout.rows[row];
        (
            self.display_to_content(index),
            row.wrapped && index == row.end,
        )
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.valid(offset);
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.moved();
        cx.notify();
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        let mut utf8 = 0;
        for ch in self.content.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(i, _)| (i < offset).then_some(i))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(i, _)| (i > offset).then_some(i))
            .unwrap_or(self.content.len())
    }

    fn previous_word(&self, offset: usize) -> usize {
        if self.masked {
            return 0;
        }
        self.content[..offset]
            .split_word_bound_indices()
            .rev()
            .find(|(_, w)| w.chars().any(char::is_alphanumeric))
            .map_or(0, |(i, _)| i)
    }

    fn next_word(&self, offset: usize) -> usize {
        if self.masked {
            return self.content.len();
        }
        self.content[offset..]
            .split_word_bound_indices()
            .find(|(_, w)| w.chars().any(char::is_alphanumeric))
            .map_or(self.content.len(), |(i, w)| offset + i + w.len())
    }

    /// The word at `offset`, for a double click: between words, the space or mark that was hit.
    /// A masked field gives everything, so it does not show where its words are.
    fn word_at(&self, offset: usize) -> Range<usize> {
        if self.masked {
            return 0..self.content.len();
        }
        self.content
            .split_word_bound_indices()
            .map(|(start, word)| start..start + word.len())
            .find(|word| offset < word.end)
            .unwrap_or(self.content.len()..self.content.len())
    }

    /// The line of text at `offset`, from one line break to the next, for a triple click.
    fn paragraph_at(&self, offset: usize) -> Range<usize> {
        let offset = self.valid(offset);
        let start = self.content[..offset].rfind('\n').map_or(0, |at| at + 1);
        let end = self.content[offset..]
            .find('\n')
            .map_or(self.content.len(), |at| offset + at);
        start..end
    }

    /// The text as drawn: dots for masked fields. One dot per character.
    fn display_text(&self) -> String {
        if self.masked {
            "•".repeat(self.content.chars().count())
        } else {
            self.content.to_string()
        }
    }

    /// Maps a content byte offset to the drawn text's byte offset.
    fn content_to_display(&self, offset: usize) -> usize {
        if self.masked {
            self.content[..offset].chars().count() * '•'.len_utf8()
        } else {
            offset
        }
    }

    /// Maps a drawn text byte offset back to a content byte offset.
    fn display_to_content(&self, offset: usize) -> usize {
        if self.masked {
            let chars = offset / '•'.len_utf8();
            self.content
                .char_indices()
                .nth(chars)
                .map_or(self.content.len(), |(i, _)| i)
        } else {
            offset
        }
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        if self.masked {
            return None;
        }
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let new_text = self.cleaned(new_text);
        let new_text = self.fitting(&new_text, &range);
        let mut letters = new_text.chars();
        let typing = range.is_empty()
            && matches!((letters.next(), letters.next()), (Some(letter), None) if !letter.is_whitespace());
        // A composition was remembered when it began.
        if self.marked_range.is_none() {
            self.remember(typing);
        }
        let mut content =
            Zeroizing::new(String::with_capacity(self.content.len() + new_text.len()));
        content.push_str(&self.content[..range.start]);
        content.push_str(new_text);
        content.push_str(&self.content[range.end..]);
        let caret = range.start + new_text.len();
        self.content = content;
        self.selected_range = caret..caret;
        self.selection_reversed = false;
        self.marked_range = None;
        self.moved();
        self.typing = typing;
        cx.emit(TextFieldEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        if self.marked_range.is_none() {
            self.remember(false);
        }
        let mut content =
            Zeroizing::new(String::with_capacity(self.content.len() + new_text.len()));
        content.push_str(&self.content[..range.start]);
        content.push_str(new_text);
        content.push_str(&self.content[range.end..]);
        self.content = content;
        self.marked_range = (!new_text.is_empty())
            .then(|| range.start + new_text.len())
            .map(|end| range.start..end);
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map_or_else(
                || range.start + new_text.len()..range.start + new_text.len(),
                |r| r.start + range.start..r.end + range.start,
            );
        self.selection_reversed = false;
        self.moved();
        cx.emit(TextFieldEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let start = layout.position(self.content_to_display(range.start), false);
        let end = layout.position(self.content_to_display(range.end), true);
        let width = if end.y == start.y {
            (end.x - start.x).max(px(0.))
        } else {
            px(0.)
        };
        Some(Bounds::new(
            bounds.origin - self.scroll + start,
            size(width, layout.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let local = self.last_bounds?.localize(&point)? + self.scroll;
        let layout = self.layout()?;
        let index = layout.index_in_row(layout.row_at(local.y), local.x);
        Some(self.offset_to_utf16(self.display_to_content(index)))
    }
}

/// One row of text on screen: a line of the text, or a part of one that wraps.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// Which of the layout's lines the row belongs to.
    line: usize,
    /// Where that line starts in the drawn text, in bytes.
    line_start: usize,
    /// Where the row starts and ends in the drawn text, in bytes.
    start: usize,
    end: usize,
    /// How far into its line the row starts.
    start_x: Pixels,
    /// The row ends where the line wraps, not where it ends.
    wrapped: bool,
}

/// The drawn text of a field, shaped and wrapped, with the places of its rows.
struct FieldLayout {
    lines: SmallVec<[WrappedLine; 1]>,
    /// Never empty.
    rows: Vec<Row>,
    line_height: Pixels,
    /// The length of the drawn text, in bytes.
    len: usize,
}

impl FieldLayout {
    fn new(lines: SmallVec<[WrappedLine; 1]>, line_height: Pixels) -> Option<Self> {
        let mut rows = Vec::new();
        let mut line_start = 0;
        for (index, line) in lines.iter().enumerate() {
            let layout = &line.unwrapped_layout;
            let mut start = 0;
            let breaks = line.wrap_boundaries().iter().filter_map(|wrap| {
                Some(
                    layout
                        .runs
                        .get(wrap.run_ix)?
                        .glyphs
                        .get(wrap.glyph_ix)?
                        .index,
                )
            });
            for (end, wrapped) in breaks.map(|end| (end, true)).chain([(line.len(), false)]) {
                rows.push(Row {
                    line: index,
                    line_start,
                    start: line_start + start,
                    end: line_start + end,
                    start_x: layout.x_for_index(start),
                    wrapped,
                });
                start = end;
            }
            line_start += line.len() + '\n'.len_utf8();
        }
        let len = line_start.saturating_sub('\n'.len_utf8());
        (!rows.is_empty()).then_some(Self {
            lines,
            rows,
            line_height,
            len,
        })
    }

    /// The row that `index` is on. An index where a line wraps is the end of one row and the
    /// start of the next: `at_row_end` picks the first.
    fn row_of(&self, index: usize, at_row_end: bool) -> usize {
        let row = self
            .rows
            .partition_point(|row| row.start <= index)
            .saturating_sub(1);
        if at_row_end && row > 0 && self.rows[row].start == index && self.rows[row - 1].wrapped {
            row - 1
        } else {
            row
        }
    }

    /// How far from the left of row `row` the place `index` is.
    fn x_in_row(&self, row: usize, index: usize) -> Pixels {
        let row = self.rows[row];
        let layout = &self.lines[row.line].unwrapped_layout;
        layout.x_for_index(index.clamp(row.start, row.end) - row.line_start) - row.start_x
    }

    /// The place in row `row` nearest to `x`.
    fn index_in_row(&self, row: usize, x: Pixels) -> usize {
        let row = self.rows[row];
        let layout = &self.lines[row.line].unwrapped_layout;
        let index = layout.closest_index_for_x(x.max(px(0.)) + row.start_x);
        (row.line_start + index).clamp(row.start, row.end)
    }

    /// The row at height `y`, or the nearest one.
    fn row_at(&self, y: Pixels) -> usize {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let row = (y / self.line_height).floor().max(0.) as usize;
        row.min(self.rows.len() - 1)
    }

    /// Where `index` is, from the top left of the text.
    fn position(&self, index: usize, at_row_end: bool) -> Point<Pixels> {
        let row = self.row_of(index, at_row_end);
        point(self.x_in_row(row, index), self.row_top(row))
    }

    fn row_top(&self, row: usize) -> Pixels {
        #[allow(clippy::cast_precision_loss)]
        let rows = row as f32;
        self.line_height * rows
    }

    fn height(&self) -> Pixels {
        self.row_top(self.rows.len())
    }

    /// The width of the widest line before wrapping.
    fn width(&self) -> Pixels {
        self.lines
            .iter()
            .map(|line| line.unwrapped_layout.width)
            .fold(px(0.), Pixels::max)
    }
}

struct TextElement {
    field: Entity<TextField>,
}

struct PrepaintState {
    layout: Option<FieldLayout>,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
    scroll: Point<Pixels>,
}

/// The width text wraps at in a field `width` wide.
fn wrap_width(width: Pixels) -> Pixels {
    (width - px(CARET_ROOM)).max(px(1.))
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let field = self.field.read(cx);
        let line_height = window.line_height();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let Some(max_lines) = field.max_lines else {
            style.size.height = line_height.into();
            return (window.request_layout(style, [], cx), ());
        };
        // As tall as its text, which depends on the width the field gets.
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let text: SharedString = if field.content.is_empty() {
            field.placeholder.clone()
        } else {
            field.content.to_string().into()
        };
        let run = text_style.to_run(text.len());
        let layout = window.request_measured_layout(style, move |known, available, window, _| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            let rows = window
                .text_system()
                .shape_text(
                    text.clone(),
                    font_size,
                    std::slice::from_ref(&run),
                    width.map(wrap_width),
                    None,
                )
                .map_or(1, |lines| {
                    lines
                        .iter()
                        .map(|line| line.wrap_boundaries().len() + 1)
                        .sum()
                });
            #[allow(clippy::cast_precision_loss)]
            let rows = rows.clamp(1, max_lines) as f32;
            size(width.unwrap_or_default(), line_height * rows)
        });
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> PrepaintState {
        let colors = theme(cx);
        let field = self.field.read(cx);
        let style = window.text_style();
        let (text, color) = if field.content.is_empty() {
            (field.placeholder.to_string(), colors.muted)
        } else {
            (field.display_text(), style.color)
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match field.marked_range.as_ref() {
            Some(marked) if !field.content.is_empty() => {
                let (start, end) = (
                    field.content_to_display(marked.start),
                    field.content_to_display(marked.end),
                );
                vec![
                    TextRun {
                        len: start,
                        ..run.clone()
                    },
                    TextRun {
                        len: end - start,
                        underline: Some(UnderlineStyle {
                            color: Some(run.color),
                            thickness: px(1.0),
                            wavy: false,
                        }),
                        ..run.clone()
                    },
                    TextRun {
                        len: text.len() - end,
                        ..run
                    },
                ]
                .into_iter()
                .filter(|r| r.len > 0)
                .collect()
            }
            _ => vec![run],
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let wrap = field.max_lines.map(|_| wrap_width(bounds.size.width));
        let layout = window
            .text_system()
            .shape_text(text.into(), font_size, &runs, wrap, None)
            .ok()
            .and_then(|lines| FieldLayout::new(lines, line_height));
        let Some(layout) = layout else {
            return PrepaintState {
                layout: None,
                cursor: None,
                selection: Vec::new(),
                scroll: Point::default(),
            };
        };

        let selected = field.content_to_display(field.selected_range.start)
            ..field.content_to_display(field.selected_range.end);
        let caret = if field.content.is_empty() {
            Point::default()
        } else {
            layout.position(
                field.content_to_display(field.cursor_offset()),
                field.caret_at_row_end,
            )
        };
        let scroll = if field.max_lines.is_some() {
            // Several lines scroll down, and only to follow the caret when it has moved: the
            // wheel can look elsewhere in the text.
            let furthest = (layout.height() - bounds.size.height).max(px(0.));
            let mut y = field.scroll.y;
            if field.reveal_caret {
                y = y
                    .min(caret.y)
                    .max(caret.y + line_height - bounds.size.height);
            }
            point(px(0.), y.clamp(px(0.), furthest))
        } else {
            // One line scrolls sideways just enough to keep the caret in view.
            let room = bounds.size.width - px(CARET_ROOM);
            let width = layout.width();
            let x = if width <= room {
                px(0.)
            } else {
                let mut x = field.scroll.x;
                if caret.x - x > room {
                    x = caret.x - room;
                }
                x.min(caret.x).clamp(px(0.), width - room)
            };
            point(x, px(0.))
        };
        let origin = bounds.origin - scroll;
        let (selection, cursor) = if selected.is_empty() || field.content.is_empty() {
            let caret = fill(
                Bounds::new(origin + caret, size(px(1.5), line_height)),
                colors.caret,
            );
            (Vec::new(), Some(caret))
        } else {
            let rows = layout.rows.iter().enumerate();
            let quads = rows.filter_map(|(index, row)| {
                let from = selected.start.max(row.start);
                let to = selected.end.min(row.end);
                if from > to {
                    return None;
                }
                let left = layout.x_in_row(index, from);
                let mut right = layout.x_in_row(index, to);
                // A selected line break shows as a little space after the row.
                if !row.wrapped && selected.end > row.end {
                    right += px(LINE_BREAK_WIDTH);
                }
                (right > left).then(|| {
                    let top = origin.y + layout.row_top(index);
                    fill(
                        Bounds::from_corners(
                            point(origin.x + left, top),
                            point(origin.x + right, top + line_height),
                        ),
                        colors.selection,
                    )
                })
            });
            (quads.collect(), None)
        };
        self.field.update(cx, |field, _| {
            field.scroll = scroll;
            field.reveal_caret = false;
        });
        PrepaintState {
            layout: Some(layout),
            cursor,
            selection,
            scroll,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        prepaint: &mut PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.field.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.field.clone()),
            cx,
        );
        // Heard anywhere in the window, so a drag goes on selecting outside the field.
        let field = self.field.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                field.update(cx, |field, cx| field.on_drag(event, cx));
            }
        });
        let Some(layout) = prepaint.layout.take() else {
            return;
        };
        // Nothing is drawn outside the field, however long the text.
        let focused = focus_handle.is_focused(window);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            // Only the field with the keyboard shows a selection, so two fields never both
            // look selected.
            for selection in prepaint.selection.drain(..) {
                if focused {
                    window.paint_quad(selection);
                }
            }
            let mut origin = bounds.origin - prepaint.scroll;
            for line in &layout.lines {
                let _ = line.paint(
                    origin,
                    layout.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                origin.y += line.size(layout.line_height).height;
            }
            // It only blinks while the window is in front; behind another window it stays on.
            if focused
                && (!window.is_window_active() || self.field.read(cx).blink.visible())
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });
        self.field.update(cx, |field, _| {
            field.last_layout = Some(layout);
            field.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        let focused = self.focus_handle.is_focused(window);
        let blinking = focused && window.is_window_active();
        self.blink
            .run(blinking, |field: &mut Self| &mut field.blink, cx);
        let context = if self.max_lines.is_some() {
            format!("{CONTEXT} {AREA_CONTEXT}")
        } else {
            CONTEXT.to_owned()
        };
        div()
            .flex()
            .items_center()
            .key_context(context.as_str())
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::close_menu))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_mouse_down))
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_prev))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .w_full()
            .when(!self.borderless, |d| {
                d.h(px(32.))
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(if focused {
                        colors.accent
                    } else {
                        colors.border
                    })
                    .bg(colors.input)
                    .text_color(colors.text)
                    .text_size(px(14.))
                    .line_height(px(20.))
            })
            .child(TextElement { field: cx.entity() })
            .children(self.menu.map(|position| self.render_menu(position, cx)))
    }
}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Key bindings for text fields (Windows and Linux style).
pub fn key_bindings() -> Vec<gpui::KeyBinding> {
    use gpui::KeyBinding;
    let ctx = Some(CONTEXT);
    let area = Some(AREA_CONTEXT);
    vec![
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("ctrl-delete", DeleteWordRight, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("ctrl-left", WordLeft, ctx),
        KeyBinding::new("ctrl-right", WordRight, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, ctx),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, ctx),
        KeyBinding::new("ctrl-a", SelectAll, ctx),
        KeyBinding::new("ctrl-v", Paste, ctx),
        KeyBinding::new("shift-insert", Paste, ctx),
        KeyBinding::new("ctrl-c", Copy, ctx),
        KeyBinding::new("ctrl-insert", Copy, ctx),
        KeyBinding::new("ctrl-x", Cut, ctx),
        KeyBinding::new("shift-delete", Cut, ctx),
        KeyBinding::new("ctrl-z", Undo, ctx),
        KeyBinding::new("ctrl-y", Redo, ctx),
        KeyBinding::new("ctrl-shift-z", Redo, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("shift-home", SelectHome, ctx),
        KeyBinding::new("shift-end", SelectEnd, ctx),
        KeyBinding::new("ctrl-home", DocStart, ctx),
        KeyBinding::new("ctrl-end", DocEnd, ctx),
        KeyBinding::new("ctrl-shift-home", SelectDocStart, ctx),
        KeyBinding::new("ctrl-shift-end", SelectDocEnd, ctx),
        KeyBinding::new("enter", Submit, ctx),
        KeyBinding::new("escape", CloseMenu, ctx),
        KeyBinding::new("tab", FocusNext, ctx),
        KeyBinding::new("shift-tab", FocusPrev, ctx),
        KeyBinding::new("shift-enter", Newline, area),
        KeyBinding::new("up", Up, area),
        KeyBinding::new("down", Down, area),
        KeyBinding::new("shift-up", SelectUp, area),
        KeyBinding::new("shift-down", SelectDown, area),
    ]
}
