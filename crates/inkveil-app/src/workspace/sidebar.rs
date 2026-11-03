//! The note sidebar: sections, note rows, right-click menus and trash.

use std::collections::HashSet;

use gpui::{
    AnyElement, Context, Entity, Focusable, IntoElement, MouseButton, MouseDownEvent, Pixels,
    Point, SharedString, Window, anchored, deferred, div, prelude::*, px,
};
use inkveil_store::{NoteId, NoteMeta, NoteSummary, SectionId, now_ms};

use super::{
    Workspace,
    note_editor::display_title,
    sections::{self, Section},
};
use crate::{
    assets::Icon,
    storage::state_keys,
    theme::theme,
    ui::{
        icon, icon_button,
        text_field::{TextField, TextFieldEvent},
    },
};

/// Notes shown per group before "Show more".
const GROUP_PREVIEW: usize = 10;

/// What the inline section name field is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionEdit {
    New,
    Rename(SectionId),
}

/// Sidebar state kept on the workspace.
#[derive(Default)]
pub struct SidebarState {
    pub sections: Vec<Section>,
    /// Groups showing all their notes (`None` is the unsorted group).
    pub expanded: HashSet<Option<SectionId>>,
    pub section_field: Option<(SectionEdit, Entity<TextField>)>,
    pub note_menu: Option<(NoteId, Point<Pixels>)>,
    pub section_menu: Option<(SectionId, Point<Pixels>)>,
}

impl Workspace {
    // ----- Section operations ------------------------------------------------------------------

    fn save_sections(&self) {
        self.storage.put_state(
            state_keys::SECTIONS,
            sections::encode(&self.sidebar.sections),
        );
    }

    pub(super) fn start_section_edit(
        &mut self,
        edit: SectionEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar.note_menu = None;
        self.sidebar.section_menu = None;
        let current = match edit {
            SectionEdit::New => String::new(),
            SectionEdit::Rename(id) => self
                .sidebar
                .sections
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.name.clone())
                .unwrap_or_default(),
        };
        let field = cx.new(|cx| {
            let mut field = TextField::new("Section name", window, cx);
            field.set_text(&current, cx);
            field
        });
        cx.subscribe_in(
            &field,
            window,
            |this, field, event, window, cx| match event {
                TextFieldEvent::Submitted | TextFieldEvent::Blurred => {
                    let name = field.read(cx).text().trim().to_owned();
                    this.finish_section_edit(&name, window, cx);
                }
                TextFieldEvent::Changed => {}
            },
        )
        .detach();
        window.focus(&field.focus_handle(cx), cx);
        self.sidebar.section_field = Some((edit, field));
        cx.notify();
    }

    fn finish_section_edit(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((edit, _)) = self.sidebar.section_field.take() else {
            return;
        };
        if !name.is_empty() {
            match edit {
                SectionEdit::New => self.sidebar.sections.push(Section {
                    id: sections::new_id(),
                    name: name.to_owned(),
                    collapsed: false,
                }),
                SectionEdit::Rename(id) => {
                    if let Some(section) = self.sidebar.sections.iter_mut().find(|s| s.id == id) {
                        name.clone_into(&mut section.name);
                    }
                }
            }
            self.save_sections();
        }
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn toggle_section(&mut self, id: SectionId, cx: &mut Context<Self>) {
        if let Some(section) = self.sidebar.sections.iter_mut().find(|s| s.id == id) {
            section.collapsed = !section.collapsed;
            self.save_sections();
            cx.notify();
        }
    }

    /// Deletes a section; its notes move to the unsorted group (nothing is deleted).
    fn delete_section(&mut self, id: SectionId, cx: &mut Context<Self>) {
        self.sidebar.section_menu = None;
        self.sidebar.sections.retain(|s| s.id != id);
        let affected: Vec<NoteId> = self
            .notes
            .iter()
            .filter(|n| n.meta.section == Some(id))
            .map(|n| n.id)
            .collect();
        for note in affected {
            self.update_note_meta(note, |m| m.section = None, cx);
        }
        self.save_sections();
        cx.notify();
    }

    fn move_note(&mut self, note: NoteId, section: Option<SectionId>, cx: &mut Context<Self>) {
        self.sidebar.note_menu = None;
        self.update_note_meta(note, |m| m.section = section, cx);
        cx.notify();
    }

    /// Changes a note's metadata through its open tab (so the tab's next save keeps the change)
    /// or directly in storage when it is not open.
    pub(super) fn update_note_meta(
        &mut self,
        id: NoteId,
        f: impl FnOnce(&mut NoteMeta),
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tabs.iter().find(|t| t.read(cx).id == id).cloned() {
            tab.update(cx, |note, cx| note.update_meta(f, cx));
            let meta = tab.read(cx).meta().clone();
            if let Some(summary) = self.notes.iter_mut().find(|n| n.id == id) {
                summary.meta = meta;
            }
        } else if let Some(summary) = self.notes.iter_mut().find(|n| n.id == id) {
            f(&mut summary.meta);
            self.storage.save_meta(id, summary.meta.clone());
        }
    }

    // ----- Rendering ---------------------------------------------------------------------------

    fn query(&self, cx: &gpui::App) -> String {
        self.search.read(cx).text().trim().to_lowercase()
    }

    fn matches(&self, note: &NoteSummary, query: &str) -> bool {
        query.is_empty()
            || self.body_hits.contains_key(&note.id)
            || note.meta.title.to_lowercase().contains(query)
            || note.meta.preview.to_lowercase().contains(query)
            || note
                .meta
                .tags
                .iter()
                .any(|t| t.to_lowercase().contains(query))
    }

    /// Notes in the trash or not, filtered by the search query, pinned first then newest.
    fn listed(&self, query: &str) -> Vec<&NoteSummary> {
        let mut notes: Vec<&NoteSummary> = self
            .notes
            .iter()
            .filter(|n| n.meta.trashed_ms.is_some() == self.show_trash)
            .filter(|n| self.matches(n, query))
            .collect();
        notes.sort_by(|a, b| {
            b.meta
                .pinned
                .cmp(&a.meta.pinned)
                .then(b.meta.modified_ms.cmp(&a.meta.modified_ms))
        });
        notes
    }

    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        let query = self.query(cx);
        let trash_count = self
            .notes
            .iter()
            .filter(|n| n.meta.trashed_ms.is_some())
            .count();
        let notes = self.listed(&query);
        let known: HashSet<SectionId> = self.sidebar.sections.iter().map(|s| s.id).collect();
        let now = now_ms();

        let mut list: Vec<AnyElement> = Vec::new();
        if self.show_trash || !query.is_empty() {
            // Flat list: trash, or search results across all sections.
            for note in &notes {
                list.push(self.note_row(note, now, cx));
            }
        } else {
            if let Some((SectionEdit::New, field)) = &self.sidebar.section_field {
                list.push(div().px_2().py_1().child(field.clone()).into_any_element());
            }
            // Unsorted notes (including ones whose section no longer exists) come first.
            let unsorted: Vec<&NoteSummary> = notes
                .iter()
                .copied()
                .filter(|n| n.meta.section.is_none_or(|s| !known.contains(&s)))
                .collect();
            self.push_group(&mut list, None, &unsorted, now, cx);
            for section in &self.sidebar.sections {
                list.push(self.section_header(section, cx));
                if !section.collapsed {
                    let in_section: Vec<&NoteSummary> = notes
                        .iter()
                        .copied()
                        .filter(|n| n.meta.section == Some(section.id))
                        .collect();
                    if in_section.is_empty() {
                        list.push(
                            div()
                                .pl(px(26.))
                                .py_1()
                                .text_size(px(12.))
                                .text_color(colors.muted)
                                .child("No notes yet")
                                .into_any_element(),
                        );
                    }
                    self.push_group(&mut list, Some(section.id), &in_section, now, cx);
                }
            }
        }

        let empty_message = if !self.loaded || !notes.is_empty() {
            None
        } else if self.show_trash {
            Some("The trash is empty. Notes here are deleted for good after 30 days.")
        } else if query.is_empty() {
            Some("No notes yet. Press Ctrl+N or double-click here to write one.")
        } else {
            Some("No matching notes.")
        };

        div()
            .w(px(260.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(colors.surface)
            .border_r_1()
            .border_color(colors.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(px(44.))
                    .child(
                        gpui::img("images/inkveil.png")
                            .size(px(20.))
                            .rounded_sm()
                            .flex_none(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(if self.show_trash { "Trash" } else { "Inkveil" }),
                    )
                    .when(!self.show_trash, |d| {
                        d.child(
                            icon_button("new-section", Icon::FolderPlus, false, "New section", cx)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.start_section_edit(SectionEdit::New, window, cx);
                                })),
                        )
                        .child(
                            icon_button("new-note", Icon::Plus, false, "New note (Ctrl+N)", cx)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.new_note(window, cx)),
                                ),
                        )
                    }),
            )
            .child(div().px_2().pb_2().child(self.search.clone()))
            .child(
                div()
                    .id("note-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .pb_2()
                    .children(list)
                    .children(empty_message.map(|text| {
                        div()
                            .px_3()
                            .py_2()
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child(text)
                    }))
                    // Double-click empty space to start a note, like the tab strip.
                    .when(!self.show_trash, |d| {
                        d.child(
                            div()
                                .id("note-list-empty")
                                .flex_1()
                                .min_h(px(48.))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(Self::double_click_new_note),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(colors.border)
                    .child(self.trash_toggle(trash_count, cx))
                    .child(
                        div().pr_2().child(
                            icon_button(
                                "open-settings",
                                Icon::Settings,
                                self.show_settings && self.settings_from_sidebar,
                                "Settings (auto-lock, AI key, editor)",
                                cx,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_settings =
                                    !this.show_settings || !this.settings_from_sidebar;
                                this.settings_from_sidebar = true;
                                cx.notify();
                            })),
                        ),
                    )
                    .when(self.show_settings && self.settings_from_sidebar, |d| {
                        d.child(
                            div().absolute().top_0().left(px(8.)).child(
                                deferred(
                                    anchored()
                                        .anchor(gpui::Anchor::BottomLeft)
                                        .snap_to_window()
                                        .child(self.render_settings_menu(cx)),
                                )
                                .with_priority(1),
                            ),
                        )
                    }),
            )
            .children(self.render_note_menu(cx))
            .children(self.render_section_menu(cx))
    }

    /// The "Trash (n)" / "Back to notes" switch at the bottom of the sidebar.
    fn trash_toggle(&self, trash_count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        div()
            .id("trash-toggle")
            .flex_1()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .h(px(34.))
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(colors.muted)
            .hover(|d| d.bg(colors.hover))
            .child(
                icon(
                    if self.show_trash {
                        Icon::FileText
                    } else {
                        Icon::Trash
                    },
                    colors.muted,
                )
                .size(px(14.)),
            )
            .child(if self.show_trash {
                "Back to notes".to_owned()
            } else {
                format!("Trash ({trash_count})")
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_trash = !this.show_trash;
                cx.notify();
            }))
    }

    /// Adds a group's notes to `list`, with "Show more" / "Show less" for long groups.
    fn push_group(
        &self,
        list: &mut Vec<AnyElement>,
        group: Option<SectionId>,
        notes: &[&NoteSummary],
        now: i64,
        cx: &mut Context<Self>,
    ) {
        let colors = theme(cx);
        let expanded = self.sidebar.expanded.contains(&group);
        let shown = if expanded {
            notes.len()
        } else {
            notes.len().min(GROUP_PREVIEW)
        };
        for note in &notes[..shown] {
            list.push(self.note_row(note, now, cx));
        }
        if notes.len() > GROUP_PREVIEW {
            let label = if expanded {
                "Show less".to_owned()
            } else {
                format!("Show {} more", notes.len() - GROUP_PREVIEW)
            };
            let key = group.map_or_else(|| "unsorted".to_owned(), |g| hex(&g));
            list.push(
                div()
                    .id(SharedString::from(format!("more-{key}")))
                    .mx_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .hover(|d| d.bg(colors.hover).text_color(colors.text))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.sidebar.expanded.remove(&group) {
                            this.sidebar.expanded.insert(group);
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
    }

    fn section_header(&self, section: &Section, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme(cx);
        let id = section.id;
        let key = hex(&id);
        let renaming =
            matches!(&self.sidebar.section_field, Some((SectionEdit::Rename(r), _)) if *r == id);
        if renaming && let Some((_, field)) = &self.sidebar.section_field {
            return div()
                .px_2()
                .pt_3()
                .pb_1()
                .child(field.clone())
                .into_any_element();
        }
        div()
            .id(SharedString::from(format!("section-{key}")))
            .group("section-header")
            .flex()
            .items_center()
            .gap_1()
            .mt_2()
            .mx_1()
            .pl_2()
            .pr_1()
            .h(px(28.))
            .rounded_md()
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(colors.muted)
            .hover(|d| d.bg(colors.hover).text_color(colors.text))
            .child(
                icon(
                    if section.collapsed {
                        Icon::ChevronRight
                    } else {
                        Icon::ChevronDown
                    },
                    colors.muted,
                )
                .size(px(12.)),
            )
            .child(div().flex_1().truncate().child(section.name.clone()))
            .child(
                div()
                    .invisible()
                    .group_hover("section-header", gpui::Styled::visible)
                    .flex()
                    .child(
                        icon_button(
                            SharedString::from(format!("section-new-{key}")),
                            Icon::Plus,
                            false,
                            "New note in this section",
                            cx,
                        )
                        .size(px(22.))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.new_note_in(Some(id), window, cx);
                            },
                        )),
                    )
                    .child(
                        icon_button(
                            SharedString::from(format!("section-more-{key}")),
                            Icon::Ellipsis,
                            false,
                            "Section options",
                            cx,
                        )
                        .size(px(22.))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.sidebar.section_menu = Some((id, event.position));
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(id, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.sidebar.section_menu = Some((id, event.position));
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    fn note_row(&self, note: &NoteSummary, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme(cx);
        let id = note.id;
        let active_id = self.active.map(|a| self.tabs[a].read(cx).id);
        let selected = active_id == Some(id);
        let confirming = self.confirm_delete.is_some_and(|(p, _)| p == id);
        let in_section = note.meta.section.is_some() && !self.show_trash;
        let pinned = note.meta.pinned && !self.show_trash;
        let has_title = !note.meta.title.trim().is_empty();
        let preview = note.meta.preview.lines().next().unwrap_or("");
        let subtitle = if has_title && !preview.is_empty() {
            format!("{} · {preview}", relative_time(note.meta.modified_ms, now))
        } else {
            relative_time(note.meta.modified_ms, now)
        };
        let searching = !self.search.read(cx).text().trim().is_empty();
        let hit = self.body_hits.get(&id).filter(|_| searching).cloned();
        let subtitle: AnyElement = match &hit {
            Some(hit) => gpui::StyledText::new(hit.snippet.clone())
                .with_highlights([(
                    hit.highlight.clone(),
                    gpui::HighlightStyle {
                        color: Some(colors.text),
                        background_color: Some(colors.find_match),
                        font_weight: Some(gpui::FontWeight::SEMIBOLD),
                        ..Default::default()
                    },
                )])
                .into_any_element(),
            None => subtitle.into_any_element(),
        };

        let row = div()
            .id(SharedString::from(format!("note-{id}")))
            .flex()
            .items_center()
            .gap_2()
            .mx_1()
            .when(in_section, |d| d.ml(px(18.)))
            .px_2()
            .py(px(5.))
            .rounded_md()
            .cursor_pointer()
            .when(selected, |d| d.bg(colors.selected))
            // Pinned notes stand out: a faint accent background and an accent bar on the left.
            .when(pinned && !selected, |d| d.bg(colors.accent.opacity(0.08)))
            .when(!selected, |d| d.hover(|d| d.bg(colors.hover)))
            .when(pinned, |d| {
                d.relative().child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(5.))
                        .bottom(px(5.))
                        .w(px(3.))
                        .rounded_full()
                        .bg(colors.accent),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .when(pinned, |d| {
                                d.child(icon(Icon::Pin, colors.accent).size(px(12.)))
                            })
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.))
                                    .when(selected || pinned, |d| {
                                        d.font_weight(gpui::FontWeight::MEDIUM)
                                    })
                                    .child(display_title(&note.meta)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .truncate()
                            .child(subtitle),
                    ),
            );

        if self.show_trash {
            row.child(
                icon_button(
                    SharedString::from(format!("restore-{id}")),
                    Icon::Restore,
                    false,
                    "Restore",
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.restore_note(id, cx))),
            )
            .child(
                icon_button(
                    SharedString::from(format!("delete-{id}")),
                    Icon::Trash,
                    confirming,
                    if confirming {
                        "Click again to delete forever"
                    } else {
                        "Delete forever"
                    },
                    cx,
                )
                .when(confirming, |b| b.text_color(colors.error))
                .on_click(cx.listener(move |this, _, _, cx| this.delete_forever(id, cx))),
            )
            .into_any_element()
        } else {
            row.on_click(cx.listener(move |this, _, window, cx| {
                let query = this.search.read(cx).text().trim().to_owned();
                if this.body_hits.contains_key(&id) && !query.is_empty() {
                    this.open_note_and_find(id, query, window, cx);
                } else {
                    this.open_note(id, None, true, window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.sidebar.section_menu = None;
                    this.sidebar.note_menu = Some((id, event.position));
                    cx.notify();
                }),
            )
            .into_any_element()
        }
    }

    fn menu_panel(&self, id: &'static str, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let colors = theme(cx);
        div()
            .id(id)
            .occlude()
            .flex()
            .flex_col()
            .p_1()
            .min_w(px(210.))
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .shadow_md()
            .text_size(px(13.))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.sidebar.note_menu = None;
                this.sidebar.section_menu = None;
                cx.notify();
            }))
    }

    fn menu_item(
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        which: Option<Icon>,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = theme(cx);
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_2()
            .h(px(28.))
            .px_2()
            .rounded_sm()
            .cursor_pointer()
            .text_color(colors.text)
            .hover(|d| d.bg(colors.hover))
            .child(
                div()
                    .w(px(16.))
                    .flex_none()
                    .children(which.map(|i| icon(i, colors.icon).size(px(14.)))),
            )
            .child(label.into())
    }

    fn render_note_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, position) = self.sidebar.note_menu?;
        let note = self.notes.iter().find(|n| n.id == id)?;
        let colors = theme(cx);
        let pinned = note.meta.pinned;
        let current = note.meta.section;
        let mut panel = self
            .menu_panel("note-menu", cx)
            .child(
                Self::menu_item("menu-open", "Open", Some(Icon::FileText), cx).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.sidebar.note_menu = None;
                        this.open_note(id, None, true, window, cx);
                    }),
                ),
            )
            .child(
                Self::menu_item(
                    "menu-pin",
                    if pinned { "Unpin" } else { "Pin to top" },
                    Some(Icon::Pin),
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sidebar.note_menu = None;
                    this.update_note_meta(id, |m| m.pinned = !pinned, cx);
                    cx.notify();
                })),
            )
            .child(div().h(px(1.)).my_1().bg(colors.border))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("MOVE TO"),
            );
        if current.is_some() {
            panel = panel.child(
                Self::menu_item("menu-unsorted", "No section", Some(Icon::FileText), cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.move_note(id, None, cx))),
            );
        }
        for (i, section) in self.sidebar.sections.iter().enumerate() {
            if Some(section.id) == current {
                continue;
            }
            let target = section.id;
            panel = panel.child(
                Self::menu_item(
                    ("menu-move", i),
                    section.name.clone(),
                    Some(Icon::Folder),
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.move_note(id, Some(target), cx);
                })),
            );
        }
        panel = panel
            .child(
                Self::menu_item(
                    "menu-new-section",
                    "New section…",
                    Some(Icon::FolderPlus),
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.start_section_edit(SectionEdit::New, window, cx);
                })),
            )
            .child(div().h(px(1.)).my_1().bg(colors.border))
            .child(
                Self::menu_item("menu-trash", "Move to trash", Some(Icon::Trash), cx).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.sidebar.note_menu = None;
                        this.trash_note(id, window, cx);
                    }),
                ),
            );
        Some(
            deferred(anchored().position(position).snap_to_window().child(panel))
                .with_priority(2)
                .into_any_element(),
        )
    }

    fn render_section_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, position) = self.sidebar.section_menu?;
        let panel =
            self.menu_panel("section-menu", cx)
                .child(
                    Self::menu_item("section-menu-new", "New note here", Some(Icon::Plus), cx)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.sidebar.section_menu = None;
                            this.new_note_in(Some(id), window, cx);
                        })),
                )
                .child(
                    Self::menu_item("section-menu-rename", "Rename", Some(Icon::Type), cx)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_section_edit(SectionEdit::Rename(id), window, cx);
                        })),
                )
                .child(
                    Self::menu_item(
                        "section-menu-delete",
                        "Delete section (keeps its notes)",
                        Some(Icon::Trash),
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.delete_section(id, cx))),
                );
        Some(
            deferred(anchored().position(position).snap_to_window().child(panel))
                .with_priority(2)
                .into_any_element(),
        )
    }
}

fn hex(id: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    id.iter().fold(String::with_capacity(32), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// "just now", "5 min ago", "3 h ago", "Yesterday", "4 days ago", "Oct 12" or "Oct 12, 2025".
pub fn relative_time(ms: i64, now: i64) -> String {
    use chrono::{Datelike, Local, TimeZone};

    let age = now - ms;
    if age < 60_000 {
        return "just now".into();
    }
    if age < 60 * 60_000 {
        return format!("{} min ago", age / 60_000);
    }
    let (Some(then), Some(today)) = (
        Local.timestamp_millis_opt(ms).single(),
        Local.timestamp_millis_opt(now).single(),
    ) else {
        return String::new();
    };
    let days = today
        .date_naive()
        .signed_duration_since(then.date_naive())
        .num_days();
    match days {
        0 => format!("{} h ago", age / (60 * 60_000)),
        1 => "Yesterday".into(),
        2..=6 => format!("{days} days ago"),
        _ if then.year() == today.year() => then.format("%b %-d").to_string(),
        _ => then.format("%b %-d, %Y").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times() {
        let now = chrono::Local::now().timestamp_millis();
        assert_eq!(relative_time(now - 10_000, now), "just now");
        assert_eq!(relative_time(now - 5 * 60_000, now), "5 min ago");
        assert_eq!(relative_time(now - 59 * 60_000, now), "59 min ago");
        // Older than a week: a calendar date such as "Oct 14".
        assert_ne!(relative_time(now - 9 * 24 * 60 * 60_000, now), "");
        assert_eq!(relative_time(now - 3 * 24 * 60 * 60_000, now), "3 days ago");
    }
}
