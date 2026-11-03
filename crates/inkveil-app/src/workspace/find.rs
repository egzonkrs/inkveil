//! Find and replace inside the open note (Ctrl+F, Ctrl+H), like VS Code's find widget.

use gpui::{
    AnyElement, Context, Entity, Focusable, IntoElement, KeyBinding, Window, actions, div,
    prelude::*, px,
};
use inkveil_doc::{FindOptions, Match};

use super::note_editor::NoteEditor;
use crate::{
    assets::Icon,
    theme::theme,
    ui::{
        icon_button,
        text_field::{TextField, TextFieldEvent},
    },
};

actions!(
    find,
    [
        Find,
        FindReplace,
        FindNext,
        FindPrev,
        CloseFind,
        ToggleMatchCase,
        ToggleWholeWord
    ]
);

/// Key context of an open note.
pub const NOTE_CONTEXT: &str = "NoteEditor";
/// Key context of the find bar.
const BAR_CONTEXT: &str = "FindBar";
/// Selected text longer than this is not copied into the find field.
const MAX_PREFILL: usize = 200;

pub fn key_bindings() -> Vec<KeyBinding> {
    let note = Some(NOTE_CONTEXT);
    let bar = Some(BAR_CONTEXT);
    vec![
        KeyBinding::new("ctrl-f", Find, note),
        KeyBinding::new("ctrl-h", FindReplace, note),
        KeyBinding::new("f3", FindNext, note),
        KeyBinding::new("shift-f3", FindPrev, note),
        KeyBinding::new("escape", CloseFind, note),
        KeyBinding::new("shift-enter", FindPrev, bar),
        KeyBinding::new("alt-c", ToggleMatchCase, bar),
        KeyBinding::new("alt-w", ToggleWholeWord, bar),
    ]
}

/// The find bar of one open note.
pub struct FindBar {
    query: Entity<TextField>,
    replacement: Entity<TextField>,
    show_replace: bool,
    options: FindOptions,
    /// In reading order.
    matches: Vec<Match>,
    current: Option<usize>,
    /// How many results the last "Replace all" replaced, until the search changes.
    replaced: Option<usize>,
}

impl NoteEditor {
    /// Opens the find bar (with the replace row for Ctrl+H) and focuses the search field.
    /// Short selected text becomes the search.
    pub fn open_find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prefill = {
            let editor = self.editor.read(cx).editor();
            let selection = editor.selection();
            (selection.is_within_one_text() && !selection.is_collapsed())
                .then(|| editor.selected_plain_text())
                .filter(|t| t.len() <= MAX_PREFILL && !t.contains('\n'))
        };
        if self.find.is_none() {
            let query = cx.new(|cx| TextField::new("Find", window, cx));
            let replacement = cx.new(|cx| TextField::new("Replace", window, cx));
            cx.subscribe_in(&query, window, |this, _, event, window, cx| match event {
                TextFieldEvent::Changed => this.refresh_find(true, cx),
                TextFieldEvent::Submitted => this.find_step(true, window, cx),
                TextFieldEvent::Blurred => {}
            })
            .detach();
            cx.subscribe_in(&replacement, window, |this, _, event, window, cx| {
                if *event == TextFieldEvent::Submitted {
                    this.replace_current(window, cx);
                }
            })
            .detach();
            self.find = Some(FindBar {
                query,
                replacement,
                show_replace: false,
                options: FindOptions::default(),
                matches: Vec::new(),
                current: None,
                replaced: None,
            });
        }
        let Some(find) = self.find.as_mut() else {
            return;
        };
        find.show_replace |= replace;
        let query = find.query.clone();
        if let Some(text) = prefill {
            query.update(cx, |field, cx| field.set_text(&text, cx));
        }
        window.focus(&query.focus_handle(cx), cx);
        self.refresh_find(true, cx);
    }

    /// Opens the find bar searching for `text` from the start of the note (from the sidebar
    /// search).
    pub fn find_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |e, cx| {
            e.set_caret(inkveil_doc::Pos::new(0, 0), cx);
        });
        self.open_find(false, window, cx);
        if let Some(find) = &self.find {
            let query = find.query.clone();
            query.update(cx, |field, cx| field.set_text(text, cx));
        }
        self.refresh_find(true, cx);
    }

    pub fn find_open(&self) -> bool {
        self.find.is_some()
    }

    /// Searches the search text again (after typing, an option change or an edit). `jump`
    /// selects the first result at or after the caret.
    pub fn refresh_find(&mut self, jump: bool, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut() else {
            return;
        };
        let query = find.query.read(cx).text().to_owned();
        let editor = self.editor.read(cx).editor();
        find.replaced = None;
        find.matches = editor.doc().find(&query, find.options);
        find.current = if find.matches.is_empty() {
            None
        } else if jump {
            let (start, _) = editor.selection().ordered();
            let after = find.matches.iter().position(|m| {
                inkveil_doc::Pos {
                    loc: m.loc,
                    offset: m.range.start,
                } >= start
            });
            Some(after.unwrap_or(0))
        } else {
            Some(find.current.unwrap_or(0).min(find.matches.len() - 1))
        };
        let (matches, current) = (find.matches.clone(), find.current);
        let selected = current.and_then(|i| matches.get(i)).cloned();
        self.editor.update(cx, |e, cx| {
            e.set_find_highlights(matches, current, cx);
            if jump && let Some(m) = selected {
                e.select_range(m.loc, m.range, cx);
            }
        });
        cx.notify();
    }

    /// Moves to the next (or previous) result, wrapping around.
    fn find_step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.is_none() {
            self.open_find(false, window, cx);
            return;
        }
        let Some(find) = self.find.as_mut() else {
            return;
        };
        let count = find.matches.len();
        if count == 0 {
            return;
        }
        let next = match find.current {
            Some(i) if forward => (i + 1) % count,
            Some(i) => (i + count - 1) % count,
            None => 0,
        };
        find.current = Some(next);
        let (matches, m) = (find.matches.clone(), find.matches[next].clone());
        self.editor.update(cx, |e, cx| {
            e.set_find_highlights(matches, Some(next), cx);
            e.select_range(m.loc, m.range, cx);
        });
        cx.notify();
    }

    fn replace_current(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_ref() else {
            return;
        };
        let Some(m) = find.current.and_then(|i| find.matches.get(i)).cloned() else {
            return;
        };
        let replacement = find.replacement.read(cx).text().to_owned();
        self.editor.update(cx, |e, cx| {
            e.run(cx, |ed| ed.replace_matches(&[m], &replacement));
        });
        self.refresh_find(true, cx);
    }

    fn replace_all(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_ref() else {
            return;
        };
        if find.matches.is_empty() {
            return;
        }
        let matches = find.matches.clone();
        let replacement = find.replacement.read(cx).text().to_owned();
        self.editor.update(cx, |e, cx| {
            e.run(cx, |ed| ed.replace_matches(&matches, &replacement));
        });
        self.refresh_find(false, cx);
        if let Some(find) = self.find.as_mut() {
            find.replaced = Some(matches.len());
        }
    }

    pub fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Escape closes a picture shown large first, then the AI suggestion, then the find bar.
        if self.zoom_open() {
            self.close_zoom(window, cx);
            return;
        }
        if self.picture_menu.take().is_some() {
            cx.notify();
            return;
        }
        if self.ai_open() {
            self.close_ai(window, cx);
            return;
        }
        if self.find.take().is_none() {
            cx.propagate();
            return;
        }
        self.editor.update(cx, |e, cx| {
            e.set_find_highlights(Vec::new(), None, cx);
            e.focus(window, cx);
        });
        cx.notify();
    }

    fn toggle_option(&mut self, f: impl FnOnce(&mut FindOptions), cx: &mut Context<Self>) {
        if let Some(find) = self.find.as_mut() {
            f(&mut find.options);
            self.refresh_find(true, cx);
        }
    }

    // ----- Actions -----------------------------------------------------------------------------

    pub(super) fn on_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        self.open_find(false, window, cx);
    }

    pub(super) fn on_find_replace(
        &mut self,
        _: &FindReplace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_find(true, window, cx);
    }

    pub(super) fn on_find_next(
        &mut self,
        _: &FindNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.find_step(true, window, cx);
    }

    pub(super) fn on_find_prev(
        &mut self,
        _: &FindPrev,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.find_step(false, window, cx);
    }

    pub(super) fn on_close_find(
        &mut self,
        _: &CloseFind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_find(window, cx);
    }

    pub(super) fn on_toggle_match_case(
        &mut self,
        _: &ToggleMatchCase,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_option(|o| o.match_case = !o.match_case, cx);
    }

    pub(super) fn on_toggle_whole_word(
        &mut self,
        _: &ToggleWholeWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_option(|o| o.whole_word = !o.whole_word, cx);
    }

    // ----- Rendering ---------------------------------------------------------------------------

    /// The floating find bar at the top right of the editor.
    pub fn render_find(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let find = self.find.as_ref()?;
        let colors = theme(cx);
        let count = match (find.current, find.matches.len(), find.replaced) {
            (_, 0, Some(n)) => format!("Replaced {n}"),
            (_, 0, None) if find.query.read(cx).text().is_empty() => String::new(),
            (_, 0, None) => "No results".to_owned(),
            (Some(i), n, _) => format!("{} of {n}", i + 1),
            (None, n, _) => format!("{n} found"),
        };
        let no_results = !find.query.read(cx).text().is_empty()
            && find.matches.is_empty()
            && find.replaced.is_none();
        let has_matches = !find.matches.is_empty();
        let show_replace = find.show_replace;
        let options = find.options;

        let search_row = div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                icon_button(
                    "toggle-replace",
                    if show_replace {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    },
                    false,
                    "Toggle replace (Ctrl+H)",
                    cx,
                )
                .size(px(24.))
                .on_click(cx.listener(|this, _, window, cx| {
                    if let Some(find) = this.find.as_mut() {
                        find.show_replace = !find.show_replace;
                        let field = find.replacement.clone();
                        if find.show_replace {
                            window.focus(&field.focus_handle(cx), cx);
                        }
                        cx.notify();
                    }
                })),
            )
            .child(
                div()
                    .w(px(240.))
                    .when(no_results, |d| {
                        d.rounded_md().border_1().border_color(colors.error)
                    })
                    .child(find.query.clone()),
            )
            .child(
                icon_button(
                    "match-case",
                    Icon::CaseSensitive,
                    options.match_case,
                    "Match case (Alt+C)",
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_option(|o| o.match_case = !o.match_case, cx);
                })),
            )
            .child(
                icon_button(
                    "whole-word",
                    Icon::WholeWord,
                    options.whole_word,
                    "Match whole word (Alt+W)",
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_option(|o| o.whole_word = !o.whole_word, cx);
                })),
            )
            .child(
                div()
                    .min_w(px(72.))
                    .px_1()
                    .text_size(px(12.))
                    .text_color(if no_results {
                        colors.error
                    } else {
                        colors.muted
                    })
                    .child(count),
            )
            .child(
                icon_button(
                    "find-prev",
                    Icon::ArrowUp,
                    false,
                    "Previous match (Shift+Enter)",
                    cx,
                )
                .when(!has_matches, |d| d.opacity(0.4))
                .on_click(cx.listener(|this, _, window, cx| this.find_step(false, window, cx))),
            )
            .child(
                icon_button(
                    "find-next",
                    Icon::ArrowDown,
                    false,
                    "Next match (Enter)",
                    cx,
                )
                .when(!has_matches, |d| d.opacity(0.4))
                .on_click(cx.listener(|this, _, window, cx| this.find_step(true, window, cx))),
            )
            .child(
                icon_button("close-find", Icon::X, false, "Close (Escape)", cx)
                    .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            );

        let replace_row = show_replace.then(|| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .pl(px(28.))
                .child(div().w(px(240.)).child(find.replacement.clone()))
                .child(
                    small_button("replace-one", Icon::Replace, "Replace", has_matches, cx)
                        .tooltip(|_, cx| crate::ui::Tooltip::view("Replace this match (Enter)", cx))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.replace_current(window, cx)),
                        ),
                )
                .child(
                    small_button(
                        "replace-all",
                        Icon::ReplaceAll,
                        "Replace all",
                        has_matches,
                        cx,
                    )
                    .tooltip(|_, cx| {
                        crate::ui::Tooltip::view("Replace every match (Ctrl+Z undoes it)", cx)
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.replace_all(window, cx))),
                )
        });

        Some(
            div()
                .id("find-bar")
                .key_context(BAR_CONTEXT)
                .occlude()
                .absolute()
                .top(px(6.))
                .right(px(28.))
                .flex()
                .flex_col()
                .gap_1()
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(colors.border)
                .bg(colors.surface)
                .shadow_md()
                .child(search_row)
                .children(replace_row)
                .into_any_element(),
        )
    }
}

/// A compact labelled button for the replace row.
fn small_button(
    id: &'static str,
    icon_name: Icon,
    label: &'static str,
    enabled: bool,
    cx: &mut Context<NoteEditor>,
) -> gpui::Stateful<gpui::Div> {
    let colors = theme(cx);
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
        .hover(|d| d.bg(colors.hover))
        .when(!enabled, |d| d.opacity(0.4))
        .child(crate::ui::icon(icon_name, colors.icon).size(px(14.)))
        .child(label)
}
