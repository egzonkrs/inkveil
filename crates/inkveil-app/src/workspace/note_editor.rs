//! One open note: title, tags, formatting toolbar and the editor.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, Focusable, Hsla, IntoElement, Pixels, Point,
    Render, SharedString, Window, anchored, deferred, div, prelude::*, px,
};
use inkveil_doc::{Align, Document, FontSize, MarkKind, Pos, Rgb, TextKind};
use inkveil_store::{NoteId, NoteMeta, now_ms};
use zeroize::Zeroizing;

use crate::{
    ai::AiMode,
    assets::Icon,
    editor::{EditorEvent, EditorView, MisspelledWord},
    spell,
    storage::Storage,
    theme::{self, theme},
    ui::{
        button, hairline, icon, icon_button,
        text_field::{TextField, TextFieldEvent},
        toolbar_separator,
    },
};

/// Save once typing pauses this long...
const SAVE_AFTER_PAUSE: Duration = Duration::from_millis(400);
/// ...or at the latest this long after the first unsaved change.
const SAVE_AT_LEAST_EVERY: Duration = Duration::from_millis(1500);
/// Keep a history snapshot this often while a note is being edited.
const SNAPSHOT_EVERY: Duration = Duration::from_mins(5);

const TEXT_COLORS: &[(&str, Option<u32>)] = &[
    ("Default", None),
    ("Gray", Some(0x8a8f98)),
    ("Red", Some(0xd1242f)),
    ("Orange", Some(0xd9730d)),
    ("Yellow", Some(0xb58a00)),
    ("Green", Some(0x2e9e5b)),
    ("Blue", Some(0x2f6fdb)),
    ("Purple", Some(0x8250df)),
    ("Pink", Some(0xc93c8f)),
];

const HIGHLIGHTS: &[(&str, Option<u32>)] = &[
    ("None", None),
    ("Yellow", Some(0xffd400)),
    ("Green", Some(0x5ad17a)),
    ("Blue", Some(0x58a6ff)),
    ("Purple", Some(0xb48cff)),
    ("Pink", Some(0xff7ab8)),
    ("Red", Some(0xff6b6b)),
    ("Orange", Some(0xffa94d)),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Menu {
    BlockStyle,
    TextColor,
    Highlight,
    Size,
    Align,
    Insert,
    Link,
}

/// Events for the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteEditorEvent {
    /// Title, tags or pin state changed (the sidebar shows them).
    MetaChanged,
    /// The document or metadata changed and needs saving.
    Dirty,
    /// The user asked to move this note to the trash.
    TrashRequested,
    /// A word was added to the personal dictionary.
    DictionaryChanged,
    /// "Improve with AI" needs an API key first.
    AiSettingsRequested,
    /// Something went wrong that the user should see (e.g. a picture could not be read).
    Problem(SharedString),
}

/// A table edit offered in the context menu.
type TableCommand = fn(&mut inkveil_doc::Editor);

/// The right-click menu.
struct ContextMenu {
    position: Point<Pixels>,
    misspelled: Option<MisspelledWord>,
    suggestions: Vec<String>,
    in_table: bool,
    has_selection: bool,
}

/// A save the workspace should hand to storage.
pub struct PendingSave {
    pub id: NoteId,
    pub meta: NoteMeta,
    pub body: Zeroizing<Vec<u8>>,
    pub snapshot: bool,
}

// Independent save-state flags; grouping them into an enum would hide which combinations occur.
#[allow(clippy::struct_excessive_bools)]
pub struct NoteEditor {
    pub id: NoteId,
    meta: NoteMeta,
    title: Entity<TextField>,
    tags: Entity<TextField>,
    pub(super) editor: Entity<EditorView>,
    link_field: Entity<TextField>,
    open_menu: Option<Menu>,
    context_menu: Option<ContextMenu>,
    pub(super) storage: Storage,
    pub(super) history: Option<super::history::History>,
    pub(super) find: Option<super::find::FindBar>,
    pub(super) ai: Option<super::ai_panel::AiPanel>,
    pub(super) zoom: Option<super::pictures::Zoom>,
    pub(super) picture_menu: Option<super::pictures::PictureMenu>,
    word_count: usize,
    saved_version: u64,
    meta_dirty: bool,
    /// Title or tags were edited: counts as a modification, unlike pinning or moving.
    title_or_tags_changed: bool,
    first_unsaved: Option<Instant>,
    /// Saves handed to storage but not yet confirmed on disk.
    in_flight: usize,
    /// After a failed save, wait until then before trying again (unless forced).
    retry_after: Option<Instant>,
    last_edit: Instant,
    last_snapshot: Instant,
    changed_since_snapshot: bool,
    /// The preview changed in the last save; the sidebar should refresh.
    preview_changed: bool,
}

impl EventEmitter<NoteEditorEvent> for NoteEditor {}

impl NoteEditor {
    pub fn new(
        id: NoteId,
        meta: NoteMeta,
        doc: Document,
        storage: Storage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let word_count = doc.word_count();
        let title = cx.new(|cx| {
            let placeholder = display_title(&NoteMeta {
                title: String::new(),
                ..meta.clone()
            });
            let mut field = TextField::new(placeholder, window, cx).borderless();
            field.set_text(&meta.title, cx);
            field
        });
        let tags = cx.new(|cx| {
            let mut field =
                TextField::new("Add tags, separated by commas…", window, cx).borderless();
            field.set_text(&meta.tags.join(", "), cx);
            field
        });
        let link_field = cx.new(|cx| TextField::new("https://…", window, cx));
        let editor = cx.new(|cx| EditorView::new(doc, cx));

        cx.subscribe_in(
            &title,
            window,
            |this, field, event, window, cx| match event {
                TextFieldEvent::Changed => {
                    this.meta.title = field.read(cx).text().trim().to_owned();
                    this.title_or_tags_changed = true;
                    this.mark_meta_dirty(cx);
                }
                TextFieldEvent::Submitted => this.editor.update(cx, |e, cx| e.focus(window, cx)),
                TextFieldEvent::Blurred => {}
            },
        )
        .detach();
        cx.subscribe_in(
            &tags,
            window,
            |this, field, event, window, cx| match event {
                TextFieldEvent::Changed => {
                    this.meta.tags = parse_tags(field.read(cx).text());
                    this.title_or_tags_changed = true;
                    this.mark_meta_dirty(cx);
                }
                TextFieldEvent::Submitted => this.editor.update(cx, |e, cx| e.focus(window, cx)),
                TextFieldEvent::Blurred => {}
            },
        )
        .detach();
        cx.subscribe_in(&link_field, window, |this, field, event, window, cx| {
            if *event == TextFieldEvent::Submitted {
                let url = field.read(cx).text().to_owned();
                this.apply_link(&url, window, cx);
            }
        })
        .detach();
        cx.subscribe_in(&editor, window, |this, _, event, window, cx| match event {
            EditorEvent::Changed => {
                this.word_count = this.editor.read(cx).doc().word_count();
                if this.find_open() {
                    this.refresh_find(false, cx);
                }
                this.mark_unsaved();
                cx.emit(NoteEditorEvent::Dirty);
                cx.notify();
            }
            EditorEvent::SelectionChanged => cx.notify(),
            EditorEvent::EditLinkRequested => this.open_link_menu(window, cx),
            EditorEvent::ImageClicked(index) => this.open_zoom(*index, window, cx),
            EditorEvent::ImageMenu { index, position } => {
                this.context_menu = None;
                this.open_picture_menu(*index, *position, cx);
            }
            EditorEvent::PasteImage(bytes) => this.insert_picture(bytes.clone(), window, cx),
            EditorEvent::ContextMenu {
                position,
                misspelled,
            } => {
                let editor = this.editor.read(cx).editor();
                this.context_menu = Some(ContextMenu {
                    position: *position,
                    suggestions: misspelled
                        .as_ref()
                        .map(|m| spell::suggest(&m.word))
                        .unwrap_or_default(),
                    misspelled: misspelled.clone(),
                    in_table: editor.in_table(),
                    has_selection: !editor.selection().is_collapsed(),
                });
                this.open_menu = None;
                cx.notify();
            }
        })
        .detach();

        Self {
            id,
            meta,
            title,
            tags,
            editor,
            link_field,
            open_menu: None,
            context_menu: None,
            storage,
            history: None,
            find: None,
            ai: None,
            zoom: None,
            picture_menu: None,
            word_count,
            saved_version: 0,
            meta_dirty: false,
            title_or_tags_changed: false,
            first_unsaved: None,
            in_flight: 0,
            retry_after: None,
            last_edit: Instant::now(),
            last_snapshot: Instant::now(),
            changed_since_snapshot: false,
            preview_changed: false,
        }
    }

    pub fn meta(&self) -> &NoteMeta {
        &self.meta
    }

    pub fn display_title(&self) -> SharedString {
        display_title(&self.meta)
    }

    /// Re-checks spelling after the dictionary loaded or changed.
    pub fn refresh_spelling(&self, cx: &mut App) {
        self.editor
            .update(cx, super::super::editor::EditorView::refresh_spelling);
    }

    pub fn doc<'a>(&self, cx: &'a App) -> &'a Document {
        self.editor.read(cx).doc()
    }

    pub fn caret(&self, cx: &App) -> Pos {
        self.editor.read(cx).editor().selection().head
    }

    pub fn word_count(&self) -> usize {
        self.word_count
    }

    /// Whether the last save changed the preview (and so maybe the displayed title).
    pub fn take_preview_changed(&mut self) -> bool {
        std::mem::take(&mut self.preview_changed)
    }

    /// Shows the note's first line as the title placeholder while it has no title.
    pub fn sync_title_placeholder(&self, cx: &mut App) {
        let placeholder = display_title(&NoteMeta {
            title: String::new(),
            ..self.meta.clone()
        });
        self.title
            .update(cx, |field, cx| field.set_placeholder(placeholder, cx));
    }

    /// Line and column of the caret, counted like the line number gutter.
    pub fn line_col(&self, cx: &App) -> (usize, usize) {
        let editor = self.editor.read(cx).editor();
        let doc = editor.doc();
        let caret = editor.selection().head;
        let mut line = 0;
        for block in &doc.blocks()[..caret.loc.block.min(doc.len())] {
            line += match block.as_text() {
                Some(t) => t.text.as_str().matches('\n').count() + 1,
                None => 1,
            };
        }
        let text = doc
            .text(caret.loc)
            .map_or("", inkveil_doc::RichText::as_str);
        let before = &text[..caret.offset.min(text.len())];
        if caret.loc.cell.is_none() {
            line += before.matches('\n').count();
        }
        let column = before.rsplit('\n').next().unwrap_or("").chars().count();
        (line + 1, column + 1)
    }

    /// Whether something is not yet confirmed on disk.
    pub fn has_unsaved_changes(&self) -> bool {
        self.first_unsaved.is_some() || self.in_flight > 0
    }

    /// Storage confirmed one of this note's saves.
    pub fn save_confirmed(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
    }

    /// A save failed: mark everything unsaved again so it is retried in a few seconds and, at the
    /// latest, when the app locks or quits.
    pub fn save_failed(&mut self) {
        let now = Instant::now();
        self.in_flight = self.in_flight.saturating_sub(1);
        self.first_unsaved.get_or_insert(now);
        self.meta_dirty = true;
        self.saved_version = u64::MAX;
        self.retry_after = Some(now + Duration::from_secs(5));
    }

    pub fn set_caret(&self, pos: Pos, cx: &mut App) {
        self.editor.update(cx, |e, cx| e.set_caret(pos, cx));
    }

    /// Focuses the title for a new note, or the editor otherwise.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        if self.meta.title.is_empty() && self.editor.read(cx).doc().is_empty() {
            window.focus(&self.title.focus_handle(cx), cx);
        } else {
            self.editor.update(cx, |e, cx| e.focus(window, cx));
        }
    }

    pub fn set_pinned(&mut self, pinned: bool, cx: &mut Context<Self>) {
        self.meta.pinned = pinned;
        self.mark_meta_dirty(cx);
    }

    /// Changes metadata from outside (sidebar menus) and saves it with the note.
    pub fn update_meta(&mut self, f: impl FnOnce(&mut NoteMeta), cx: &mut Context<Self>) {
        f(&mut self.meta);
        self.mark_meta_dirty(cx);
    }

    fn mark_unsaved(&mut self) {
        let now = Instant::now();
        self.first_unsaved.get_or_insert(now);
        self.last_edit = now;
        self.changed_since_snapshot = true;
    }

    fn mark_meta_dirty(&mut self, cx: &mut Context<Self>) {
        self.meta_dirty = true;
        self.mark_unsaved();
        cx.emit(NoteEditorEvent::MetaChanged);
        cx.emit(NoteEditorEvent::Dirty);
        cx.notify();
    }

    /// Returns a save if one is due. `force` saves any unsaved change immediately.
    pub(super) fn take_save(&mut self, force: bool, cx: &App) -> Option<PendingSave> {
        let first = self.first_unsaved?;
        let now = Instant::now();
        let waiting_to_retry = self.retry_after.is_some_and(|t| now < t);
        let due = force
            || !waiting_to_retry
                && (now.duration_since(self.last_edit) >= SAVE_AFTER_PAUSE
                    || now.duration_since(first) >= SAVE_AT_LEAST_EVERY);
        if !due {
            return None;
        }
        let editor = self.editor.read(cx);
        let version = editor.editor().version();
        // Only real edits update the "last modified" time; pinning or moving a note doesn't.
        if version != self.saved_version || self.title_or_tags_changed {
            self.meta.modified_ms = now_ms();
        }
        let body = editor.doc().encode();
        let preview = preview_of(&editor.doc().plain_text());
        self.saved_version = version;
        if preview != self.meta.preview {
            self.meta.preview = preview;
            self.preview_changed = true;
        }
        self.meta_dirty = false;
        self.title_or_tags_changed = false;
        self.first_unsaved = None;
        self.retry_after = None;
        self.in_flight += 1;

        let snapshot = self.changed_since_snapshot
            && (force || now.duration_since(self.last_snapshot) >= SNAPSHOT_EVERY);
        if snapshot {
            self.last_snapshot = now;
            self.changed_since_snapshot = false;
        }
        Some(PendingSave {
            id: self.id,
            meta: self.meta.clone(),
            body,
            snapshot,
        })
    }

    // ----- Toolbar -----------------------------------------------------------------------------

    fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.open_menu = if self.open_menu == Some(menu) {
            None
        } else {
            Some(menu)
        };
        cx.notify();
    }

    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_menu = None;
        self.editor.update(cx, |e, cx| e.focus(window, cx));
        cx.notify();
    }

    fn open_link_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.editor.read(cx).current_link().unwrap_or_default();
        self.link_field.update(cx, |f, cx| f.set_text(&current, cx));
        self.open_menu = Some(Menu::Link);
        window.focus(&self.link_field.focus_handle(cx), cx);
        cx.notify();
    }

    fn apply_link(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = normalize_url(url);
        self.editor.update(cx, |e, cx| e.apply_link(&url, cx));
        self.close_menu(window, cx);
    }

    fn edit<F: FnOnce(&mut EditorView, &mut Context<EditorView>)>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: F,
    ) {
        self.editor.update(cx, f);
        self.close_menu(window, cx);
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ed = self.editor.read(cx).editor();
        let block = ed.current_text_block();
        let kind = block.map_or(TextKind::Paragraph, |b| b.kind);
        let align = block.map_or(Align::Left, |b| b.align);
        let resized = block.is_some_and(|b| b.size != FontSize::Normal);
        let active_marks: Vec<MarkKind> = [
            MarkKind::Bold,
            MarkKind::Italic,
            MarkKind::Underline,
            MarkKind::Strike,
            MarkKind::Code,
        ]
        .into_iter()
        .filter(|m| ed.is_mark_active(*m))
        .collect();
        let active = |m: MarkKind| active_marks.contains(&m);
        let (can_undo, can_redo) = (ed.can_undo(), ed.can_redo());
        let has_selection = !ed.selection().is_collapsed();
        let marks = ed.caret_marks();
        let colors = theme(cx);

        let mark_button = |id: &'static str,
                           icon: Icon,
                           kind: MarkKind,
                           tip: &'static str,
                           cx: &mut Context<Self>| {
            icon_button(id, icon, active(kind), tip, cx).on_click(cx.listener(
                move |this, _, window, cx| {
                    this.edit(window, cx, |e, cx| e.toggle_mark(kind, cx));
                },
            ))
        };
        let kind_button = |id: &'static str,
                           icon: Icon,
                           target: TextKind,
                           tip: &'static str,
                           cx: &mut Context<Self>| {
            icon_button(id, icon, kind.same_kind(target), tip, cx).on_click(cx.listener(
                move |this, _, window, cx| {
                    this.edit(window, cx, |e, cx| e.set_text_kind(target, cx));
                },
            ))
        };

        let style_label = match kind {
            TextKind::Heading(1) => "Heading 1",
            TextKind::Heading(2) => "Heading 2",
            TextKind::Heading(_) => "Heading 3",
            TextKind::Quote => "Quote",
            TextKind::Code => "Code block",
            _ => "Text",
        };

        div()
            .flex()
            .items_center()
            .gap(px(2.))
            .px(px(40.))
            .h(px(40.))
            .flex_none()
            .child(
                icon_button("undo", Icon::Undo, false, "Undo (Ctrl+Z)", cx)
                    .when(!can_undo, |b| b.opacity(0.4))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.edit(window, cx, super::super::editor::EditorView::undo);
                    })),
            )
            .child(
                icon_button("redo", Icon::Redo, false, "Redo (Ctrl+Y)", cx)
                    .when(!can_redo, |b| b.opacity(0.4))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.edit(window, cx, super::super::editor::EditorView::redo);
                    })),
            )
            .child(toolbar_separator(cx))
            .child(
                self.menu_anchor(
                    Menu::BlockStyle,
                    div()
                        .id("block-style")
                        .flex()
                        .items_center()
                        .gap_1()
                        .h(px(28.))
                        .px_2()
                        .rounded_md()
                        .cursor_pointer()
                        .text_size(px(13.))
                        .text_color(colors.icon)
                        .hover(|d| d.bg(colors.hover))
                        .child(style_label)
                        .child(icon(Icon::ChevronDown, colors.icon).size(px(12.)))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.toggle_menu(Menu::BlockStyle, cx)),
                        )
                        .into_any_element(),
                    cx,
                ),
            )
            .child(toolbar_separator(cx))
            .child(mark_button(
                "bold",
                Icon::Bold,
                MarkKind::Bold,
                "Bold (Ctrl+B)",
                cx,
            ))
            .child(mark_button(
                "italic",
                Icon::Italic,
                MarkKind::Italic,
                "Italic (Ctrl+I)",
                cx,
            ))
            .child(mark_button(
                "underline",
                Icon::Underline,
                MarkKind::Underline,
                "Underline (Ctrl+U)",
                cx,
            ))
            .child(mark_button(
                "strike",
                Icon::Strikethrough,
                MarkKind::Strike,
                "Strikethrough (Ctrl+Shift+X)",
                cx,
            ))
            .child(mark_button(
                "code",
                Icon::Code,
                MarkKind::Code,
                "Inline code (Ctrl+E)",
                cx,
            ))
            .child(
                self.menu_anchor(
                    Menu::Link,
                    icon_button(
                        "link",
                        Icon::Link,
                        marks.link.is_some(),
                        "Link (Ctrl+K)",
                        cx,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.open_menu == Some(Menu::Link) {
                            this.close_menu(window, cx);
                        } else {
                            this.open_link_menu(window, cx);
                        }
                    }))
                    .into_any_element(),
                    cx,
                ),
            )
            .child(toolbar_separator(cx))
            .child(
                self.menu_anchor(
                    Menu::TextColor,
                    icon_button(
                        "text-color",
                        Icon::TextColor,
                        marks.color.is_some(),
                        "Text color",
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::TextColor, cx)))
                    .into_any_element(),
                    cx,
                ),
            )
            .child(
                self.menu_anchor(
                    Menu::Highlight,
                    icon_button(
                        "highlight",
                        Icon::Highlighter,
                        marks.highlight.is_some(),
                        "Highlight",
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Highlight, cx)))
                    .into_any_element(),
                    cx,
                ),
            )
            .child(
                self.menu_anchor(
                    Menu::Size,
                    icon_button("size", Icon::FontSize, resized, "Text size", cx)
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Size, cx)))
                        .into_any_element(),
                    cx,
                ),
            )
            .child(toolbar_separator(cx))
            .child(kind_button(
                "bullets",
                Icon::List,
                TextKind::Bullet,
                "Bulleted list (Ctrl+Shift+8)",
                cx,
            ))
            .child(kind_button(
                "numbers",
                Icon::ListOrdered,
                TextKind::Numbered,
                "Numbered list (Ctrl+Shift+7)",
                cx,
            ))
            .child(kind_button(
                "tasks",
                Icon::ListChecks,
                TextKind::Task { checked: false },
                "Checklist (Ctrl+Shift+9)",
                cx,
            ))
            .child(toolbar_separator(cx))
            .child(
                self.menu_anchor(
                    Menu::Align,
                    icon_button(
                        "align",
                        match align {
                            Align::Left => Icon::AlignLeft,
                            Align::Center => Icon::AlignCenter,
                            Align::Right => Icon::AlignRight,
                        },
                        align != Align::Left,
                        "Alignment",
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Align, cx)))
                    .into_any_element(),
                    cx,
                ),
            )
            .child(
                icon_button(
                    "outdent",
                    Icon::IndentDecrease,
                    false,
                    "Decrease indent (Shift+Tab)",
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.edit(window, cx, EditorView::outdent);
                })),
            )
            .child(
                icon_button(
                    "indent",
                    Icon::IndentIncrease,
                    false,
                    "Increase indent (Tab)",
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.edit(window, cx, EditorView::indent);
                })),
            )
            .child(toolbar_separator(cx))
            .child(
                self.menu_anchor(
                    Menu::Insert,
                    icon_button("insert", Icon::Plus, false, "Insert", cx)
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Insert, cx)))
                        .into_any_element(),
                    cx,
                ),
            )
            .child(
                icon_button(
                    "clear",
                    Icon::RemoveFormatting,
                    false,
                    "Clear formatting (Ctrl+\\)",
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.edit(
                        window,
                        cx,
                        super::super::editor::EditorView::clear_formatting,
                    );
                })),
            )
            // Improve with AI and Humanize sit at the far right end of the toolbar.
            .child(div().flex_1().min_w(px(8.)))
            .child(self.ai_button(
                AiMode::Improve,
                has_selection,
                "Improve the selected text with AI",
                cx,
            ))
            .child(self.ai_button(
                AiMode::Humanize,
                has_selection,
                "Rewrite the selection in plain, natural words, as if you typed it",
                cx,
            ))
    }

    /// The Improve or Humanize button at the right end of the toolbar.
    fn ai_button(
        &self,
        mode: AiMode,
        has_selection: bool,
        tip: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = theme(cx);
        let (id, icon_name, label) = match mode {
            AiMode::Improve => ("improve-ai", Icon::Sparkles, "Improve"),
            AiMode::Humanize => ("humanize-ai", Icon::NotebookPen, "Humanize"),
        };
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(px(28.))
            .px_2()
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(colors.text)
            .when(!has_selection, |d| d.opacity(0.55))
            .when(self.ai_mode() == Some(mode), |d| d.bg(colors.selected))
            .hover(|d| d.bg(colors.hover))
            .tooltip(move |_, cx| {
                crate::ui::Tooltip::view(
                    if has_selection {
                        tip
                    } else {
                        "Select some text first; only the selection is sent"
                    },
                    cx,
                )
            })
            .child(icon(icon_name, colors.accent).size(px(14.)))
            .child(label)
            .on_click(cx.listener(move |this, _, window, cx| match mode {
                AiMode::Improve => this.improve_selection(window, cx),
                AiMode::Humanize => this.humanize_selection(window, cx),
            }))
    }

    /// Wraps a toolbar control so its menu opens just below it.
    fn menu_anchor(
        &self,
        menu: Menu,
        control: AnyElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let open = self.open_menu == Some(menu);
        div().relative().child(control).when(open, |d| {
            d.child(
                div().absolute().top(px(32.)).left_0().child(
                    deferred(
                        anchored()
                            .snap_to_window()
                            .child(self.render_menu(menu, cx)),
                    )
                    .with_priority(1),
                ),
            )
        })
    }

    fn render_menu(&self, menu: Menu, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme(cx);
        let panel = div()
            .id("menu")
            .occlude()
            .flex()
            .flex_col()
            .p_1()
            .min_w(px(160.))
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .shadow_md()
            .text_size(px(13.))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.open_menu = None;
                cx.notify();
            }));

        match menu {
            Menu::BlockStyle => panel
                .children(
                    [
                        ("Text", TextKind::Paragraph, 14.),
                        ("Heading 1", TextKind::Heading(1), 20.),
                        ("Heading 2", TextKind::Heading(2), 17.),
                        ("Heading 3", TextKind::Heading(3), 15.),
                        ("Quote", TextKind::Quote, 14.),
                        ("Code block", TextKind::Code, 13.),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(i, (label, kind, size))| {
                        menu_row(("style", i), label, colors.text, colors.hover)
                            .text_size(px(size))
                            .when(kind == TextKind::Code, |d| d.font_family(theme::MONO_FONT))
                            .when(matches!(kind, TextKind::Heading(_)), |d| {
                                d.font_weight(gpui::FontWeight::SEMIBOLD)
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit(window, cx, |e, cx| {
                                    if kind == TextKind::Paragraph {
                                        e.set_text_kind(TextKind::Paragraph, cx);
                                    } else if !e
                                        .editor()
                                        .current_text_block()
                                        .is_some_and(|b| b.kind == kind)
                                    {
                                        e.set_text_kind(kind, cx);
                                    }
                                });
                            }))
                    }),
                )
                .into_any_element(),
            Menu::TextColor => panel
                .children(TEXT_COLORS.iter().enumerate().map(|(i, (label, color))| {
                    let swatch: Hsla = color.map_or(colors.text, |c| gpui::rgb(c).into());
                    menu_row(("color", i), label, swatch, colors.hover)
                        .child(div().size(px(10.)).rounded_full().bg(swatch).flex_none())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let color = color.map(Rgb);
                            this.edit(window, cx, |e, cx| e.set_color(color, cx));
                        }))
                }))
                .into_any_element(),
            Menu::Highlight => panel
                .children(HIGHLIGHTS.iter().enumerate().map(|(i, (label, color))| {
                    menu_row(("highlight", i), label, colors.text, colors.hover)
                        .when_some(*color, |d, c| {
                            d.child(
                                div()
                                    .size(px(12.))
                                    .rounded_sm()
                                    .bg(gpui::rgba((c << 8) | 0x99))
                                    .flex_none(),
                            )
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let color = color.map(Rgb);
                            this.edit(window, cx, |e, cx| e.set_highlight(color, cx));
                        }))
                }))
                .into_any_element(),
            Menu::Size => panel
                .children(
                    [
                        ("Small", FontSize::Small),
                        ("Normal", FontSize::Normal),
                        ("Large", FontSize::Large),
                        ("Huge", FontSize::Huge),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(i, (label, size))| {
                        menu_row(("size", i), label, colors.text, colors.hover)
                            .text_size(px(14. * size.scale()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit(window, cx, |e, cx| e.set_size(size, cx));
                            }))
                    }),
                )
                .into_any_element(),
            Menu::Align => panel
                .children(
                    [
                        ("Left", Align::Left, Icon::AlignLeft),
                        ("Center", Align::Center, Icon::AlignCenter),
                        ("Right", Align::Right, Icon::AlignRight),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(i, (label, align, which))| {
                        menu_row(("align", i), label, colors.text, colors.hover)
                            .child(icon(which, colors.icon))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit(window, cx, |e, cx| e.set_align(align, cx));
                            }))
                    }),
                )
                .into_any_element(),
            Menu::Insert => panel
                .child(
                    menu_row("insert-table", "Table (3 × 3)", colors.text, colors.hover)
                        .child(icon(Icon::Table, colors.icon))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.edit(window, cx, |e, cx| e.insert_table(3, 3, cx));
                        })),
                )
                .child(
                    menu_row("insert-divider", "Divider", colors.text, colors.hover)
                        .child(icon(Icon::Divider, colors.icon))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.edit(window, cx, super::super::editor::EditorView::insert_divider);
                        })),
                )
                .child(
                    menu_row("insert-code", "Code block", colors.text, colors.hover)
                        .child(icon(Icon::CodeBlock, colors.icon))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.edit(window, cx, |e, cx| e.set_text_kind(TextKind::Code, cx));
                        })),
                )
                .child(
                    menu_row("insert-quote", "Quote", colors.text, colors.hover)
                        .child(icon(Icon::Quote, colors.icon))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.edit(window, cx, |e, cx| e.set_text_kind(TextKind::Quote, cx));
                        })),
                )
                .child(
                    menu_row("insert-image", "Image…", colors.text, colors.hover)
                        .child(icon(Icon::Image, colors.icon))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_menu(window, cx);
                            this.choose_pictures(window, cx);
                        })),
                )
                .into_any_element(),
            Menu::Link => panel
                .w(px(320.))
                .p_2()
                .gap_2()
                .child(self.link_field.clone())
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .justify_end()
                        .child(button("remove-link", "Remove", false, cx).on_click(
                            cx.listener(|this, _, window, cx| this.apply_link("", window, cx)),
                        ))
                        .child(
                            button("apply-link", "Apply", true, cx).on_click(cx.listener(
                                |this, _, window, cx| {
                                    let url = this.link_field.read(cx).text().to_owned();
                                    this.apply_link(&url, window, cx);
                                },
                            )),
                        ),
                )
                .into_any_element(),
        }
    }
}

/// A clickable menu row without a fixed label; the caller adds the content.
fn menu_item(
    id: impl Into<gpui::ElementId>,
    color: Hsla,
    hover: Hsla,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .h(px(28.))
        .px_2()
        .rounded_sm()
        .cursor_pointer()
        .text_color(color)
        .hover(move |d| d.bg(hover))
}

/// A clickable menu row with a label (extra children go after it).
fn menu_row(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    color: Hsla,
    hover: Hsla,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .h(px(28.))
        .px_2()
        .rounded_sm()
        .cursor_pointer()
        .text_color(color)
        .hover(move |d| d.bg(hover))
        .child(label)
}

/// The title, or the note's first line when it has no title, or "Untitled".
pub fn display_title(meta: &NoteMeta) -> SharedString {
    let title = meta.title.trim();
    if !title.is_empty() {
        return title.to_owned().into();
    }
    let first_line = meta.preview.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        "Untitled".into()
    } else {
        first_line.to_owned().into()
    }
}

/// The first non-empty line of a note, cut to a sidebar-sized length.
fn preview_of(text: &str) -> String {
    const MAX_CHARS: usize = 100;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && *l != "---")
        .unwrap_or("");
    match line.char_indices().nth(MAX_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    }
}

fn parse_tags(text: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for tag in text.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        if !tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
            tags.push(tag.to_owned());
        }
    }
    tags
}

/// Adds `https://` to bare domains; keeps other schemes as typed.
fn normalize_url(url: &str) -> String {
    let url = url.trim();
    if url.is_empty() || url.contains("://") || url.starts_with("mailto:") {
        url.to_owned()
    } else if url.contains('@') && !url.contains('/') {
        format!("mailto:{url}")
    } else {
        format!("https://{url}")
    }
}

impl Render for NoteEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        let pinned = self.meta.pinned;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .key_context(super::find::NOTE_CONTEXT)
            .on_action(cx.listener(Self::on_find))
            .on_action(cx.listener(Self::on_find_replace))
            .on_action(cx.listener(Self::on_find_next))
            .on_action(cx.listener(Self::on_find_prev))
            .on_action(cx.listener(Self::on_close_find))
            .on_action(cx.listener(Self::on_toggle_match_case))
            .on_action(cx.listener(Self::on_toggle_whole_word))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .px(px(44.))
                    .pt(px(18.))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(20.))
                                    .line_height(px(30.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(self.title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(18.))
                                    .text_color(colors.muted)
                                    .child(self.tags.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                icon_button(
                                    "find",
                                    Icon::Search,
                                    self.find.is_some(),
                                    "Find in note (Ctrl+F)",
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        if this.find_open() {
                                            this.close_find(window, cx);
                                        } else {
                                            this.open_find(false, window, cx);
                                        }
                                    },
                                )),
                            )
                            .child(
                                icon_button(
                                    "pin",
                                    Icon::Pin,
                                    pinned,
                                    if pinned { "Unpin" } else { "Pin to top" },
                                    cx,
                                )
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.set_pinned(!pinned, cx)),
                                ),
                            )
                            .child(
                                icon_button(
                                    "history",
                                    Icon::History,
                                    self.history.is_some(),
                                    "Version history",
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        if this.history_open() {
                                            this.close_history(window, cx);
                                        } else {
                                            this.open_history(window, cx);
                                        }
                                    },
                                )),
                            )
                            .child(
                                icon_button("trash", Icon::Trash, false, "Move to trash", cx)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(NoteEditorEvent::TrashRequested);
                                    })),
                            ),
                    ),
            )
            .child(div().pt_3().child(hairline(cx)))
            .map(|d| match self.render_history(cx) {
                Some(history) => d.child(history),
                None => d.child(self.render_toolbar(cx)).child(hairline(cx)).child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .on_drop(cx.listener(Self::on_drop_files))
                        .child(self.editor.clone())
                        .children(self.render_find(cx))
                        .children(self.render_ai(cx)),
                ),
            })
            .children(self.render_context_menu(cx))
            .children(self.render_picture_menu(cx))
            .children(self.render_zoom(window, cx))
    }
}

impl NoteEditor {
    fn close_context_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.context_menu = None;
        self.editor.update(cx, |e, cx| e.focus(window, cx));
        cx.notify();
    }

    fn render_context_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.context_menu.as_ref()?;
        let colors = theme(cx);
        let separator = || div().h(px(1.)).mx_1().my_1().bg(colors.border);
        let mut panel = div()
            .id("context-menu")
            .occlude()
            .flex()
            .flex_col()
            .p_1()
            .min_w(px(200.))
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .shadow_md()
            .text_size(px(13.))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.context_menu = None;
                cx.notify();
            }));

        if let Some(word) = &menu.misspelled {
            if menu.suggestions.is_empty() {
                panel = panel.child(
                    div()
                        .px_2()
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .text_color(colors.muted)
                        .child("No suggestions"),
                );
            }
            for (i, suggestion) in menu.suggestions.iter().enumerate() {
                let (loc, range, text) = (word.loc, word.range.clone(), suggestion.clone());
                panel = panel.child(
                    menu_item(("suggestion", i), colors.text, colors.hover)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(suggestion.clone())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let (range, text) = (range.clone(), text.clone());
                            this.editor
                                .update(cx, |e, cx| e.replace_range(loc, range, &text, cx));
                            this.close_context_menu(window, cx);
                        })),
                );
            }
            let added = word.word.clone();
            panel = panel
                .child(
                    menu_item("add-word", colors.text, colors.hover)
                        .child(
                            div()
                                .flex_1()
                                .child(format!("Add \"{added}\" to dictionary")),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            spell::add_personal(&added);
                            cx.emit(NoteEditorEvent::DictionaryChanged);
                            this.close_context_menu(window, cx);
                        })),
                )
                .child(separator());
        }

        let has_selection = menu.has_selection;
        panel = panel
            .child(
                menu_row("ctx-cut", "Cut", colors.text, colors.hover)
                    .when(!has_selection, |d| d.opacity(0.45))
                    .child(div().text_color(colors.muted).child("Ctrl+X"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.editor.update(cx, |e, cx| e.cut_selection(window, cx));
                        this.close_context_menu(window, cx);
                    })),
            )
            .child(
                menu_row("ctx-copy", "Copy", colors.text, colors.hover)
                    .when(!has_selection, |d| d.opacity(0.45))
                    .child(div().text_color(colors.muted).child("Ctrl+C"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.editor.update(cx, |e, cx| e.copy_selection(window, cx));
                        this.close_context_menu(window, cx);
                    })),
            )
            .child(
                menu_row("ctx-paste", "Paste", colors.text, colors.hover)
                    .child(div().text_color(colors.muted).child("Ctrl+V"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.editor
                            .update(cx, |e, cx| e.paste_clipboard(window, cx));
                        this.close_context_menu(window, cx);
                    })),
            )
            .child(
                menu_row("ctx-select-all", "Select all", colors.text, colors.hover)
                    .child(div().text_color(colors.muted).child("Ctrl+A"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.editor
                            .update(cx, super::super::editor::EditorView::select_everything);
                        this.close_context_menu(window, cx);
                    })),
            )
            .child(separator())
            .child(
                menu_row("ctx-improve", "Improve with AI", colors.text, colors.hover)
                    .when(!has_selection, |d| d.opacity(0.45))
                    .child(icon(Icon::Sparkles, colors.accent).size(px(14.)))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.context_menu = None;
                        this.improve_selection(window, cx);
                    })),
            )
            .child(
                menu_row(
                    "ctx-humanize",
                    "Humanize with AI",
                    colors.text,
                    colors.hover,
                )
                .when(!has_selection, |d| d.opacity(0.45))
                .child(icon(Icon::NotebookPen, colors.accent).size(px(14.)))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.context_menu = None;
                    this.humanize_selection(window, cx);
                })),
            );

        if menu.in_table {
            let table_items: [(&'static str, &'static str, TableCommand); 5] = [
                (
                    "ctx-row",
                    "Insert row below",
                    inkveil_doc::Editor::table_insert_row_below,
                ),
                (
                    "ctx-col",
                    "Insert column right",
                    inkveil_doc::Editor::table_insert_col_right,
                ),
                (
                    "ctx-del-row",
                    "Delete row",
                    inkveil_doc::Editor::table_delete_row,
                ),
                (
                    "ctx-del-col",
                    "Delete column",
                    inkveil_doc::Editor::table_delete_col,
                ),
                (
                    "ctx-del-table",
                    "Delete table",
                    inkveil_doc::Editor::table_delete,
                ),
            ];
            panel = panel.child(separator());
            for (id, label, command) in table_items {
                panel = panel.child(menu_row(id, label, colors.text, colors.hover).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.editor.update(cx, |e, cx| e.table_command(command, cx));
                        this.close_context_menu(window, cx);
                    }),
                ));
            }
        }

        Some(
            deferred(
                anchored()
                    .position(menu.position)
                    .snap_to_window()
                    .child(panel),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}
