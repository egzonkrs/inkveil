//! Version history of a note: browse saved versions, preview one, restore it.

use chrono::{Local, TimeZone};
use gpui::{AnyElement, Context, Entity, IntoElement, SharedString, Window, div, prelude::*, px};
use inkveil_doc::Document;
use inkveil_store::{RevisionId, RevisionSummary, now_ms};

use super::{note_editor::NoteEditor, sidebar::relative_time};
use crate::{
    assets::Icon,
    editor::EditorView,
    theme::theme,
    ui::{button, icon, icon_button},
};

/// The history panel of one open note.
pub struct History {
    /// Newest first.
    revisions: Vec<RevisionSummary>,
    selected: Option<RevisionId>,
    preview: Option<Entity<EditorView>>,
    loading: bool,
    error: Option<SharedString>,
}

impl NoteEditor {
    pub fn history_open(&self) -> bool {
        self.history.is_some()
    }

    /// Opens the history panel and loads the list of saved versions.
    pub fn open_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.history = Some(History {
            revisions: Vec::new(),
            selected: None,
            preview: None,
            loading: true,
            error: None,
        });
        cx.notify();
        let storage = self.storage.clone();
        let id = self.id;
        cx.spawn_in(window, async move |this, cx| {
            let result = storage.list_revisions(id).await;
            this.update_in(cx, |this, window, cx| {
                let Some(history) = this.history.as_mut() else {
                    return;
                };
                history.loading = false;
                match result {
                    Ok(mut revisions) => {
                        revisions.reverse();
                        let first = revisions.first().map(|r| r.id);
                        history.revisions = revisions;
                        if let Some(first) = first {
                            this.select_revision(first, window, cx);
                        }
                    }
                    Err(error) => history.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn close_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.history = None;
        self.editor.update(cx, |e, cx| e.focus(window, cx));
        cx.notify();
    }

    fn select_revision(
        &mut self,
        revision: RevisionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(history) = self.history.as_mut() else {
            return;
        };
        history.selected = Some(revision);
        history.preview = None;
        cx.notify();
        let storage = self.storage.clone();
        let id = self.id;
        cx.spawn_in(window, async move |this, cx| {
            let body = storage.load_revision(id, revision).await;
            this.update(cx, |this, cx| {
                let Some(history) = this.history.as_mut() else {
                    return;
                };
                if history.selected != Some(revision) {
                    return;
                }
                match body.map(|b| Document::decode(&b)) {
                    Ok(Ok(doc)) => {
                        history.preview = Some(cx.new(|cx| EditorView::read_only(doc, cx)));
                    }
                    Ok(Err(error)) => history.error = Some(error.to_string().into()),
                    Err(error) => history.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Replaces the note with the previewed version. The current text is saved as a version
    /// first, and the restore can be undone with Ctrl+Z.
    fn restore_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self
            .history
            .as_ref()
            .and_then(|h| h.preview.as_ref())
            .map(|p| p.read(cx).doc().clone())
        else {
            return;
        };
        if let Some(save) = self.take_save(true, cx) {
            self.storage.save_note(save.id, save.meta, save.body);
        }
        self.storage.snapshot(self.id);
        self.editor
            .update(cx, |e, cx| e.run(cx, |ed| ed.replace_document(doc)));
        self.close_history(window, cx);
    }

    /// The preview pane and version list that replace the editor while history is open.
    pub fn render_history(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let history = self.history.as_ref()?;
        let colors = theme(cx);
        let now = now_ms();
        let selected = history
            .selected
            .and_then(|id| history.revisions.iter().find(|r| r.id == id));

        let banner = div()
            .flex()
            .items_center()
            .gap_3()
            .px(px(40.))
            .h(px(44.))
            .flex_none()
            .bg(colors.surface)
            .border_b_1()
            .border_color(colors.border)
            .text_size(px(13.))
            .child(icon(Icon::History, colors.accent))
            .child(div().flex_1().truncate().child(match selected {
                Some(r) => format!(
                    "Viewing the version from {} ({})",
                    absolute_time(r.meta.modified_ms),
                    relative_time(r.meta.modified_ms, now)
                ),
                None if history.loading => "Loading history…".to_owned(),
                None => "No version selected".to_owned(),
            }))
            .when(history.preview.is_some(), |d| {
                d.child(
                    button("restore-version", "Restore this version", true, cx)
                        .h(px(28.))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.restore_selected(window, cx)),
                        ),
                )
            })
            .child(
                button("back-to-current", "Back to current", false, cx)
                    .h(px(28.))
                    .on_click(cx.listener(|this, _, window, cx| this.close_history(window, cx))),
            );

        let preview: AnyElement = match (&history.preview, &history.error) {
            (_, Some(error)) => div()
                .p_6()
                .text_color(colors.error)
                .child(error.clone())
                .into_any_element(),
            (Some(preview), None) => preview.clone().into_any_element(),
            (None, None) => div()
                .p_6()
                .text_color(colors.muted)
                .child(if history.loading || history.selected.is_some() {
                    "Loading…"
                } else {
                    "Select a version on the right."
                })
                .into_any_element(),
        };

        let mut list = div()
            .id("history-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(1.))
            .p_1();
        if !history.loading && history.revisions.is_empty() {
            list = list.child(
                div()
                    .p_3()
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .child(
                        "No saved versions yet. A version is kept every few minutes while you \
                         edit, and when you close the note or the vault locks.",
                    ),
            );
        }
        for (i, revision) in history.revisions.iter().enumerate() {
            let id = revision.id;
            let is_selected = history.selected == Some(id);
            let preview_line = revision
                .meta
                .preview
                .lines()
                .next()
                .unwrap_or("")
                .to_owned();
            list = list.child(
                div()
                    .id(("revision", i))
                    .flex()
                    .flex_col()
                    .px_2()
                    .py(px(6.))
                    .rounded_md()
                    .cursor_pointer()
                    .when(is_selected, |d| d.bg(colors.selected))
                    .when(!is_selected, |d| d.hover(|d| d.bg(colors.hover)))
                    .child(
                        div()
                            .text_size(px(13.))
                            .when(is_selected, |d| d.font_weight(gpui::FontWeight::MEDIUM))
                            .child(relative_time(revision.meta.modified_ms, now)),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child(absolute_time(revision.meta.modified_ms)),
                    )
                    .when(!preview_line.is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .truncate()
                                .child(preview_line),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_revision(id, window, cx);
                    })),
            );
        }

        let panel = div()
            .w(px(300.))
            .flex_none()
            .flex()
            .flex_col()
            .bg(colors.surface)
            .border_l_1()
            .border_color(colors.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pl_3()
                    .pr_1()
                    .h(px(44.))
                    .flex_none()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Version history"),
                    )
                    .child(
                        icon_button("close-history", Icon::X, false, "Close history", cx).on_click(
                            cx.listener(|this, _, window, cx| this.close_history(window, cx)),
                        ),
                    ),
            )
            .child(list);

        Some(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(banner)
                        .child(div().flex_1().min_h_0().child(preview)),
                )
                .child(panel)
                .into_any_element(),
        )
    }
}

/// "Oct 23, 2026 19:05".
fn absolute_time(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%b %-d, %Y %H:%M").to_string())
        .unwrap_or_default()
}
