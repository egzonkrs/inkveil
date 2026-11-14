//! The tab bar and the area below it: dragging tabs to reorder them, the right-click menu, pinned
//! tabs, and two notes shown at once in a split view.

use gpui::{
    AnyElement, ClickEvent, Context, DragMoveEvent, Entity, IntoElement, MouseButton,
    MouseDownEvent, Pixels, Point, Render, SharedString, Stateful, Window, anchored, deferred, div,
    point, prelude::*, px, relative,
};
use inkveil_doc::Document;
use inkveil_store::{NoteId, NoteMeta, NoteSummary, now_ms};

use super::{
    Workspace,
    note_editor::NoteEditor,
    tab_layout::{EVEN_RATIO, Split},
};
use crate::{assets::Icon, theme::theme, ui::icon};

/// Closed tabs remembered for "Reopen closed tab".
const REMEMBER_CLOSED: usize = 25;
const MENU_WIDTH: Pixels = px(250.);
const PINNED_TAB_WIDTH: Pixels = px(38.);

/// A tab being dragged.
#[derive(Debug, Clone)]
pub(super) struct DraggedTab {
    id: NoteId,
    title: SharedString,
}

/// The bar between the two notes of a split being dragged.
#[derive(Debug, Clone, Copy)]
struct DraggedDivider;

/// What follows the mouse while a tab is dragged.
struct TabPreview {
    title: SharedString,
}

impl Render for TabPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(30.))
            .max_w(px(220.))
            .px_3()
            .rounded_md()
            .border_1()
            .border_color(colors.accent)
            .bg(colors.background)
            .shadow_md()
            .font_family(crate::theme::UI_FONT)
            .text_size(px(13.))
            .text_color(colors.text)
            .child(icon(Icon::FileText, colors.muted).size(px(14.)))
            .child(div().truncate().child(self.title.clone()))
    }
}

/// The side of the editor a dragged tab would open on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneSide {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabSubmenu {
    /// What to do with the split this tab is in.
    Arrange,
    /// Which tab to show next to this one.
    SplitWith,
}

/// The right-click menu of a tab.
pub(super) struct TabMenu {
    id: NoteId,
    position: Point<Pixels>,
    submenu: Option<TabSubmenu>,
}

/// State of the tab bar.
#[derive(Default)]
pub(super) struct TabBarState {
    menu: Option<TabMenu>,
    /// Where the dragged tab would land in the bar.
    drop_slot: Option<usize>,
    /// Where the dragged tab would open in the editor.
    drop_side: Option<PaneSide>,
    /// Most recently closed last.
    closed: Vec<NoteId>,
}

// ----- Commands ----------------------------------------------------------------------------------

impl Workspace {
    pub(super) fn order(&self, cx: &gpui::App) -> Vec<NoteId> {
        self.tabs.iter().map(|t| t.read(cx).id).collect()
    }

    pub(super) fn tab_index(&self, id: NoteId, cx: &gpui::App) -> Option<usize> {
        self.tabs.iter().position(|t| t.read(cx).id == id)
    }

    pub(super) fn active_id(&self, cx: &gpui::App) -> Option<NoteId> {
        self.active.map(|a| self.tabs[a].read(cx).id)
    }

    fn tab(&self, id: NoteId, cx: &gpui::App) -> Option<Entity<NoteEditor>> {
        self.tabs.iter().find(|t| t.read(cx).id == id).cloned()
    }

    /// Puts the tabs in `order`; the same note stays active.
    pub(super) fn apply_order(&mut self, order: &[NoteId], cx: &mut Context<Self>) {
        let active = self.active_id(cx);
        self.tabs.sort_by_key(|t| {
            let id = t.read(cx).id;
            order.iter().position(|o| *o == id)
        });
        self.active = active.and_then(|id| self.tab_index(id, cx));
        self.session_dirty = true;
        cx.notify();
    }

    /// Forgets where a tab was about to land, in case its drag ended outside any drop target.
    pub(super) fn clear_finished_drag(&mut self, cx: &gpui::App) {
        if !cx.has_active_drag() {
            self.tab_bar.drop_slot = None;
            self.tab_bar.drop_side = None;
        }
    }

    /// Remembers a tab that is closing and takes it out of its split.
    pub(super) fn forget_tab(&mut self, id: NoteId) {
        self.tab_bar.closed.retain(|c| *c != id);
        self.tab_bar.closed.push(id);
        if self.tab_bar.closed.len() > REMEMBER_CLOSED {
            self.tab_bar.closed.remove(0);
        }
        self.layout.remove(id);
    }

    /// The latest closed note that can still be opened. Notes that were trashed since are skipped.
    fn reopenable(&self, cx: &gpui::App) -> Option<NoteId> {
        self.tab_bar.closed.iter().rev().copied().find(|id| {
            self.tab_index(*id, cx).is_none()
                && self
                    .notes
                    .iter()
                    .any(|n| n.id == *id && n.meta.trashed_ms.is_none())
        })
    }

    pub(super) fn reopen_closed_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.reopenable(cx) {
            self.tab_bar.closed.retain(|c| *c != id);
            self.open_note(id, None, true, window, cx);
        }
    }

    fn close_notes(&mut self, ids: &[NoteId], window: &mut Window, cx: &mut Context<Self>) {
        for id in ids {
            if let Some(index) = self.tab_index(*id, cx) {
                self.close_tab(index, window, cx);
            }
        }
    }

    /// Shows two open notes together. Focus goes to `focus`.
    pub(super) fn split_tabs(
        &mut self,
        first: NoteId,
        second: NoteId,
        focus: NoteId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let order = self.layout.split(&self.order(cx), first, second);
        self.apply_order(&order, cx);
        if let Some(index) = self.tab_index(focus, cx) {
            self.activate(index, window, cx);
        }
    }

    /// Shows `other` beside `id`, on its right.
    fn split_with(
        &mut self,
        id: NoteId,
        other: NoteId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_tabs(id, other, other, window, cx);
    }

    /// Shows a new, empty note beside `id`.
    fn split_with_new_note(&mut self, id: NoteId, window: &mut Window, cx: &mut Context<Self>) {
        self.new_note(window, cx);
        if let Some(new) = self.active_id(cx) {
            self.split_with(id, new, window, cx);
        }
    }

    /// The sidebar's "Open in split view": shows a note beside the active one.
    pub(super) fn open_in_split(
        &mut self,
        id: NoteId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let beside = self
            .active_id(cx)
            .filter(|a| *a != id && self.layout.split_of(*a).is_none())
            .filter(|_| self.layout.split_of(id).is_none());
        match beside {
            Some(anchor) if self.tab_index(id, cx).is_some() => {
                self.split_with(anchor, id, window, cx);
            }
            Some(anchor) => {
                self.pending_split = Some((id, anchor));
                self.open_note(id, None, true, window, cx);
            }
            None => self.open_note(id, None, true, window, cx),
        }
    }

    /// Finishes [`Self::open_in_split`] once the note has been read.
    pub(super) fn finish_open_in_split(
        &mut self,
        id: NoteId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((_, anchor)) = self.pending_split.take_if(|(new, _)| *new == id) {
            self.split_with(anchor, id, window, cx);
        }
    }

    /// Ctrl+Shift+S: splits the active tab with the nearest free one (or a new note), or ends the
    /// split it is in.
    pub(super) fn toggle_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.active_id(cx) else {
            return;
        };
        if self.layout.split_of(active).is_some() {
            self.unsplit(active, cx);
        } else if let Some(other) = self.layout.nearest_free(&self.order(cx), active) {
            self.split_with(active, other, window, cx);
        } else {
            self.split_with_new_note(active, window, cx);
        }
    }

    fn unsplit(&mut self, id: NoteId, cx: &mut Context<Self>) {
        self.layout.unsplit(id);
        self.session_dirty = true;
        cx.notify();
    }

    fn swap_split(&mut self, id: NoteId, cx: &mut Context<Self>) {
        let order = self.layout.swap(&self.order(cx), id);
        self.apply_order(&order, cx);
    }

    fn toggle_tab_pin(&mut self, id: NoteId, cx: &mut Context<Self>) {
        let order = self.layout.toggle_pin(&self.order(cx), id);
        self.apply_order(&order, cx);
    }

    /// Makes a copy of the note as it is right now and opens it next to the original. Pictures are
    /// shared by both notes: the store keeps a picture while any note uses it.
    fn duplicate_note(&mut self, id: NoteId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id, cx) else {
            return;
        };
        let Ok(copy) = NoteId::generate() else {
            return;
        };
        let source = tab.read(cx);
        let doc: Document = source.doc(cx).clone();
        let now = now_ms();
        let meta = NoteMeta {
            title: format!("{} (copy)", source.display_title()),
            created_ms: now,
            modified_ms: now,
            pinned: false,
            ..source.meta().clone()
        };
        self.storage.save_note(copy, meta.clone(), doc.encode());
        self.notes.push(NoteSummary {
            id: copy,
            meta: meta.clone(),
        });
        let tab = self.make_tab(copy, meta, doc, window, cx);
        self.tabs.push(tab);
        let order = self.order(cx);
        let last = self.layout.members(id).into_iter().last().unwrap_or(id);
        let slot = order.iter().position(|o| *o == last).map_or(0, |i| i + 1);
        let order = self.layout.move_tab(&order, copy, slot);
        self.apply_order(&order, cx);
        if let Some(index) = self.tab_index(copy, cx) {
            self.activate(index, window, cx);
        }
    }

    /// Pins and splits waiting for restored tabs to open.
    pub(super) fn apply_restored_layout(&mut self, cx: &mut Context<Self>) {
        let mut order = self.order(cx);
        let waiting = std::mem::take(&mut self.restoring.pinned);
        let (ready, waiting): (Vec<_>, Vec<_>) =
            waiting.into_iter().partition(|id| order.contains(id));
        self.restoring.pinned = waiting;
        let changed = !ready.is_empty();
        for id in ready {
            if !self.layout.is_pinned(id) {
                order = self.layout.toggle_pin(&order, id);
            }
        }
        let waiting = std::mem::take(&mut self.restoring.splits);
        let (ready, waiting): (Vec<_>, Vec<_>) = waiting
            .into_iter()
            .partition(|s| order.contains(&s.first) && order.contains(&s.second));
        self.restoring.splits = waiting;
        let changed = changed || !ready.is_empty();
        for split in ready {
            order = self.layout.restore_split(&order, split);
        }
        if changed {
            self.apply_order(&order, cx);
        }
    }

    fn close_tab_menu(&mut self, cx: &mut Context<Self>) {
        self.tab_bar.menu = None;
        cx.notify();
    }

    pub(super) fn tab_bar_menu_open(&self) -> bool {
        self.tab_bar.menu.is_some()
    }

    /// Closes the tab menu (Escape). Returns whether it was open.
    pub(super) fn dismiss_tab_menu(&mut self) -> bool {
        self.tab_bar.menu.take().is_some()
    }

    fn open_tab_menu(&mut self, id: NoteId, event: &MouseDownEvent, cx: &mut Context<Self>) {
        self.tab_bar.menu = Some(TabMenu {
            id,
            position: event.position,
            submenu: None,
        });
        cx.notify();
    }

    fn set_tab_submenu(&mut self, submenu: Option<TabSubmenu>, cx: &mut Context<Self>) {
        if let Some(menu) = &mut self.tab_bar.menu
            && menu.submenu != submenu
        {
            menu.submenu = submenu;
            cx.notify();
        }
    }
}

// ----- Dragging ----------------------------------------------------------------------------------

impl Workspace {
    fn set_drop_slot(&mut self, slot: Option<usize>, cx: &mut Context<Self>) {
        if self.tab_bar.drop_slot != slot {
            self.tab_bar.drop_slot = slot;
            cx.notify();
        }
    }

    /// The mouse moved over the tab at `index` during a drag.
    fn drag_over_tab(
        &mut self,
        index: usize,
        event: &DragMoveEvent<DraggedTab>,
        cx: &mut Context<Self>,
    ) {
        if !event.bounds.contains(&event.event.position) {
            return;
        }
        let right_half = event.event.position.x > event.bounds.center().x;
        let dragged = event.drag(cx).id;
        let slot = index + usize::from(right_half);
        let slot = self.layout.drop_slot(&self.order(cx), dragged, slot);
        self.set_drop_slot(Some(slot), cx);
    }

    fn drop_on_tab_bar(&mut self, dragged: &DraggedTab, cx: &mut Context<Self>) {
        if let Some(slot) = self.tab_bar.drop_slot.take() {
            let order = self.layout.move_tab(&self.order(cx), dragged.id, slot);
            self.apply_order(&order, cx);
        }
        cx.notify();
    }

    /// Whether a dragged tab can open in a split with the note on screen.
    fn can_split_with_active(&self, dragged: NoteId, cx: &gpui::App) -> bool {
        self.active_id(cx).is_some_and(|active| {
            active != dragged
                && self.layout.split_of(active).is_none()
                && self.layout.split_of(dragged).is_none()
        })
    }

    fn drag_over_editor(&mut self, event: &DragMoveEvent<DraggedTab>, cx: &mut Context<Self>) {
        let inside = event.bounds.contains(&event.event.position);
        let side = (inside && self.can_split_with_active(event.drag(cx).id, cx)).then(|| {
            if event.event.position.x < event.bounds.center().x {
                PaneSide::Left
            } else {
                PaneSide::Right
            }
        });
        if self.tab_bar.drop_side != side {
            self.tab_bar.drop_side = side;
            cx.notify();
        }
    }

    fn drop_on_editor(
        &mut self,
        dragged: &DraggedTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let side = self.tab_bar.drop_side.take();
        if let (Some(side), Some(active)) = (side, self.active_id(cx)) {
            let (first, second) = match side {
                PaneSide::Left => (dragged.id, active),
                PaneSide::Right => (active, dragged.id),
            };
            self.split_tabs(first, second, dragged.id, window, cx);
        }
        cx.notify();
    }
}

// ----- The tab bar -------------------------------------------------------------------------------

impl Workspace {
    /// A thin line showing where a dragged tab would land, at the left edge of its parent.
    fn drop_marker(cx: &Context<Self>) -> gpui::Div {
        div()
            .absolute()
            .left(px(-1.))
            .top(px(3.))
            .bottom(px(3.))
            .w(px(3.))
            .rounded_full()
            .bg(theme(cx).accent)
    }

    fn render_tab(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let colors = theme(cx);
        let note = self.tabs[index].read(cx);
        let id = note.id;
        let title = note.display_title();
        let focused = self.active == Some(index);
        let visible = self.active_id(cx).is_some_and(|active| {
            active == id || self.layout.split_of(active).is_some_and(|s| s.has(id))
        });
        let split = self.layout.split_of(id).copied();
        let pinned = self.layout.is_pinned(id);
        let dragging = cx.has_active_drag();
        let marker = dragging && self.tab_bar.drop_slot == Some(index);
        let which = match split {
            Some(s) if s.stacked => Icon::Rows,
            Some(_) => Icon::Columns,
            None => Icon::FileText,
        };
        let preview = DraggedTab {
            id,
            title: title.clone(),
        };
        div()
            .id(SharedString::from(format!("tab-{index}")))
            .debug_selector(|| format!("tab-{index}"))
            .group("tab")
            .relative()
            .flex()
            .items_center()
            .gap_2()
            .h_full()
            .flex_none()
            .map(|d| {
                if pinned {
                    d.w(PINNED_TAB_WIDTH).justify_center()
                } else {
                    d.pl_3().pr_1().max_w(px(220.))
                }
            })
            // The two tabs of a split read as one piece: no line between them.
            .when(split.is_none_or(|s| s.second == id), |d| {
                d.border_r_1().border_color(colors.border)
            })
            .cursor_pointer()
            // The whole title on hover: a pinned tab shows none, a long one is cut short.
            .tooltip({
                let title = title.clone();
                move |_, cx| crate::ui::Tooltip::view(title.clone(), cx)
            })
            .text_size(px(13.))
            .when(visible, |d| d.bg(colors.background))
            .text_color(if focused { colors.text } else { colors.muted })
            .when(!visible, |d| d.hover(|d| d.bg(colors.hover)))
            .child(
                icon(
                    which,
                    if split.is_some() {
                        colors.accent
                    } else {
                        colors.muted
                    },
                )
                .size(px(14.)),
            )
            .when(!pinned, |d| {
                d.child(div().truncate().child(title)).child(
                    div()
                        .id(("close", index))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(20.))
                        .rounded_sm()
                        .when(!focused, |d| {
                            d.invisible().group_hover("tab", gpui::Styled::visible)
                        })
                        .hover(|d| d.bg(colors.hover))
                        .child(icon(Icon::X, colors.muted).size(px(13.)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            if let Some(index) = this.tab_index(id, cx) {
                                this.close_tab(index, window, cx);
                            }
                        })),
                )
            })
            // A line along the bottom joins the two tabs of a split; the one with the caret is
            // also marked along the top.
            .when(split.is_some(), |d| {
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(2.))
                        .bg(colors.accent.opacity(0.55)),
                )
                .when(focused, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .top_0()
                            .h(px(2.))
                            .bg(colors.accent),
                    )
                })
            })
            .when(marker, |d| d.child(Self::drop_marker(cx)))
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(index) = this.tab_index(id, cx) {
                    this.activate(index, window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    if let Some(index) = this.tab_index(id, cx) {
                        this.close_tab(index, window, cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event, _, cx| this.open_tab_menu(id, event, cx)),
            )
            .on_drag(preview, |tab, _, _, cx| {
                let title = tab.title.clone();
                cx.new(|_| TabPreview { title })
            })
            .on_drag_move(cx.listener(move |this, event, _, cx| {
                this.drag_over_tab(index, event, cx);
            }))
    }

    pub(super) fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let colors = theme(cx);
        let tabs: Vec<_> = (0..self.tabs.len())
            .map(|index| self.render_tab(index, cx))
            .collect();
        let end = self.tabs.len();
        let marker_at_end = cx.has_active_drag() && self.tab_bar.drop_slot == Some(end);
        div()
            .id("tabs")
            .flex()
            .h(px(36.))
            .flex_none()
            .bg(colors.surface)
            .border_b_1()
            .border_color(colors.border)
            .overflow_x_scroll()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, _| this.touch()))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedTab>, _, cx| {
                    if !event.bounds.contains(&event.event.position) {
                        this.set_drop_slot(None, cx);
                    }
                }),
            )
            .on_drop(cx.listener(|this, dragged: &DraggedTab, _, cx| {
                this.drop_on_tab_bar(dragged, cx);
            }))
            .children(tabs)
            .child(
                div()
                    .id("tabs-empty")
                    .debug_selector(|| "tabs-empty".to_owned())
                    .relative()
                    .flex_1()
                    .min_w(px(48.))
                    .h_full()
                    .when(marker_at_end, |d| d.child(Self::drop_marker(cx)))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::double_click_new_note))
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<DraggedTab>, _, cx| {
                            if event.bounds.contains(&event.event.position) {
                                let dragged = event.drag(cx).id;
                                let slot = this.layout.drop_slot(&this.order(cx), dragged, end);
                                this.set_drop_slot(Some(slot), cx);
                            }
                        },
                    )),
            )
    }
}

// ----- The right-click menu ----------------------------------------------------------------------

/// A menu row: icon, label and optionally a shortcut. Disabled rows are dimmed.
fn menu_row(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    which: Option<Icon>,
    hint: Option<&'static str>,
    cx: &gpui::App,
) -> Stateful<gpui::Div> {
    let colors = theme(cx);
    let id = id.into();
    div()
        .debug_selector(|| id.to_string())
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
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .children(hint.map(|h| {
            div()
                .flex_none()
                .text_size(px(12.))
                .text_color(colors.muted)
                .child(h)
        }))
}

fn menu_surface(cx: &gpui::App) -> gpui::Div {
    let colors = theme(cx);
    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(MENU_WIDTH)
        .p_1()
        .rounded_md()
        .border_1()
        .border_color(colors.border)
        .bg(colors.background)
        .shadow_md()
}

fn menu_separator(cx: &gpui::App) -> gpui::Div {
    div().h(px(1.)).my_1().bg(theme(cx).border)
}

impl Workspace {
    /// Runs a menu command and closes the menu.
    fn menu_action(
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static {
        cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.tab_bar.menu = None;
            action(this, window, cx);
            cx.notify();
        })
    }

    /// A row that does something; hovering it closes any open submenu.
    fn action_row(
        id: &'static str,
        label: &'static str,
        which: Option<Icon>,
        hint: Option<&'static str>,
        enabled: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Stateful<gpui::Div> {
        let row = menu_row(id, label, which, hint, cx).on_hover(cx.listener(
            |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.set_tab_submenu(None, cx);
                }
            },
        ));
        if enabled {
            row.on_click(Self::menu_action(cx, action))
        } else {
            row.opacity(0.45).cursor_default()
        }
    }

    /// A row that opens a submenu when hovered or clicked.
    fn submenu_row(
        id: &'static str,
        label: &'static str,
        which: Icon,
        submenu: TabSubmenu,
        open: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let colors = theme(cx);
        menu_row(id, label, Some(which), None, cx)
            .when(open, |d| d.bg(colors.hover))
            .child(icon(Icon::ChevronRight, colors.muted).size(px(14.)))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.set_tab_submenu(Some(submenu), cx);
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_tab_submenu(Some(submenu), cx);
            }))
    }

    fn render_arrange_menu(&self, split: Split<NoteId>, cx: &mut Context<Self>) -> gpui::Div {
        let id = split.first;
        let (close_first, close_second) = if split.stacked {
            ("Close top view", "Close bottom view")
        } else {
            ("Close left view", "Close right view")
        };
        let (first, second) = (split.first, split.second);
        menu_surface(cx)
            .child(
                menu_row(
                    "split-unsplit",
                    "Unsplit tabs",
                    Some(Icon::FileText),
                    None,
                    cx,
                )
                .on_click(Self::menu_action(cx, move |this, _, cx| {
                    this.unsplit(id, cx);
                })),
            )
            .child(menu_separator(cx))
            .child(
                menu_row("split-close-first", close_first, Some(Icon::X), None, cx).on_click(
                    Self::menu_action(cx, move |this, window, cx| {
                        this.close_notes(&[first], window, cx);
                    }),
                ),
            )
            .child(
                menu_row("split-close-second", close_second, Some(Icon::X), None, cx).on_click(
                    Self::menu_action(cx, move |this, window, cx| {
                        this.close_notes(&[second], window, cx);
                    }),
                ),
            )
            .child(menu_separator(cx))
            .child(
                menu_row(
                    "split-orientation",
                    if split.stacked {
                        "Show side by side"
                    } else {
                        "Show stacked"
                    },
                    Some(if split.stacked {
                        Icon::Columns
                    } else {
                        Icon::Rows
                    }),
                    None,
                    cx,
                )
                .on_click(Self::menu_action(cx, move |this, _, cx| {
                    this.layout.toggle_stacked(id);
                    this.session_dirty = true;
                    cx.notify();
                })),
            )
            .child(
                menu_row(
                    "split-swap",
                    "Swap tab positions",
                    Some(Icon::ArrowLeftRight),
                    None,
                    cx,
                )
                .on_click(Self::menu_action(cx, move |this, _, cx| {
                    this.swap_split(id, cx);
                })),
            )
    }

    fn render_split_with_menu(&self, id: NoteId, cx: &mut Context<Self>) -> gpui::Div {
        let mut panel = menu_surface(cx).child(
            menu_row("split-new-note", "New note", Some(Icon::Plus), None, cx).on_click(
                Self::menu_action(cx, move |this, window, cx| {
                    this.split_with_new_note(id, window, cx);
                }),
            ),
        );
        let others: Vec<(NoteId, SharedString)> = self
            .tabs
            .iter()
            .map(|t| t.read(cx))
            .filter(|n| n.id != id && self.layout.split_of(n.id).is_none())
            .map(|n| (n.id, n.display_title()))
            .collect();
        if !others.is_empty() {
            panel = panel.child(menu_separator(cx));
        }
        let mut list = div()
            .id("split-with-list")
            .flex()
            .flex_col()
            .max_h(px(320.))
            .overflow_y_scroll();
        for (i, (other, title)) in others.into_iter().enumerate() {
            list = list.child(
                menu_row(("split-with", i), title, Some(Icon::FileText), None, cx).on_click(
                    Self::menu_action(cx, move |this, window, cx| {
                        this.split_with(id, other, window, cx);
                    }),
                ),
            );
        }
        panel.child(list)
    }

    pub(super) fn render_tab_menu(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let menu = self.tab_bar.menu.as_ref()?;
        let (id, position, submenu) = (menu.id, menu.position, menu.submenu);
        self.tab_index(id, cx)?;
        let order = self.order(cx);
        let split = self.layout.split_of(id).copied();
        let pinned = self.layout.is_pinned(id);
        let others = !self.layout.others(&order, id).is_empty();
        let to_the_right = !self.layout.to_the_right(&order, id).is_empty();
        let reopenable = self.reopenable(cx).is_some();

        let main = menu_surface(cx)
            .child(match split {
                Some(_) => Self::submenu_row(
                    "tab-arrange",
                    "Arrange split view",
                    Icon::Columns,
                    TabSubmenu::Arrange,
                    submenu == Some(TabSubmenu::Arrange),
                    cx,
                ),
                None => Self::submenu_row(
                    "tab-split-with",
                    "Split view with",
                    Icon::Columns,
                    TabSubmenu::SplitWith,
                    submenu == Some(TabSubmenu::SplitWith),
                    cx,
                ),
            })
            .child(menu_separator(cx))
            .child(Self::action_row(
                "tab-duplicate",
                "Duplicate note",
                Some(Icon::Copy),
                None,
                true,
                cx,
                move |this, window, cx| this.duplicate_note(id, window, cx),
            ))
            .child(Self::action_row(
                "tab-pin",
                if pinned { "Unpin tab" } else { "Pin tab" },
                Some(Icon::Pin),
                None,
                true,
                cx,
                move |this, _, cx| this.toggle_tab_pin(id, cx),
            ))
            .child(menu_separator(cx))
            .child(Self::action_row(
                "tab-close",
                "Close",
                Some(Icon::X),
                Some("Ctrl+W"),
                true,
                cx,
                move |this, window, cx| this.close_notes(&[id], window, cx),
            ))
            .child(Self::action_row(
                "tab-close-others",
                "Close other tabs",
                None,
                None,
                others,
                cx,
                move |this, window, cx| {
                    let doomed = this.layout.others(&this.order(cx), id);
                    this.close_notes(&doomed, window, cx);
                },
            ))
            .child(Self::action_row(
                "tab-close-right",
                "Close tabs to the right",
                None,
                None,
                to_the_right,
                cx,
                move |this, window, cx| {
                    let doomed = this.layout.to_the_right(&this.order(cx), id);
                    this.close_notes(&doomed, window, cx);
                },
            ))
            .child(menu_separator(cx))
            .child(Self::action_row(
                "tab-reopen",
                "Reopen closed tab",
                Some(Icon::Restore),
                Some("Ctrl+Shift+T"),
                reopenable,
                cx,
                Self::reopen_closed_tab,
            ));
        let side = match (submenu, split) {
            (Some(TabSubmenu::Arrange), Some(split)) => Some(self.render_arrange_menu(split, cx)),
            (Some(TabSubmenu::SplitWith), None) => Some(self.render_split_with_menu(id, cx)),
            _ => None,
        };

        // The menu keeps its place when the submenu opens: it opens to the left if there is no
        // room on the right, so the row under the mouse never moves away.
        let available = window.viewport_size().width;
        let x = position.x.min(available - MENU_WIDTH - px(8.));
        let flip = x + MENU_WIDTH * 2. + px(8.) > available;
        let panel = div()
            .id("tab-menu")
            .occlude()
            .flex()
            .items_start()
            .gap_1()
            .when(flip, gpui::Styled::flex_row_reverse)
            .text_size(px(13.))
            .font_family(crate::theme::UI_FONT)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_tab_menu(cx)))
            .child(main)
            .children(side);
        let place = anchored().snap_to_window();
        let place = if flip {
            place
                .anchor(gpui::Anchor::TopRight)
                .position(point(x + MENU_WIDTH, position.y))
        } else {
            place.position(point(x, position.y))
        };
        Some(
            deferred(place.child(panel))
                .with_priority(2)
                .into_any_element(),
        )
    }
}

// ----- The notes below the tab bar ---------------------------------------------------------------

impl Workspace {
    /// The active note, or the two notes of its split.
    pub(super) fn render_notes(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let active = self.active_id(cx)?;
        if let Some(split) = self.layout.split_of(active).copied()
            && let (Some(first), Some(second)) =
                (self.tab(split.first, cx), self.tab(split.second, cx))
        {
            return Some(self.render_split(split, first, second, active, cx));
        }
        Some(self.render_single_note(active, cx))
    }

    /// One note. Dropping a tab on it starts a split.
    fn render_single_note(&self, active: NoteId, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme(cx);
        let side = self.tab_bar.drop_side.filter(|_| cx.has_active_drag());
        div()
            .id("single-note")
            .debug_selector(|| "single-note".to_owned())
            .relative()
            .size_full()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedTab>, _, cx| {
                    this.drag_over_editor(event, cx);
                }),
            )
            .on_drop(cx.listener(|this, dragged: &DraggedTab, window, cx| {
                this.drop_on_editor(dragged, window, cx);
            }))
            .children(self.tab(active, cx))
            .children(side.map(|side| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .w(relative(0.5))
                    .map(|d| match side {
                        PaneSide::Left => d.left_0(),
                        PaneSide::Right => d.right_0(),
                    })
                    .bg(colors.accent.opacity(0.14))
                    .border_2()
                    .border_color(colors.accent)
            }))
            .into_any_element()
    }

    fn render_split(
        &self,
        split: Split<NoteId>,
        first: Entity<NoteEditor>,
        second: Entity<NoteEditor>,
        active: NoteId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme(cx);
        let stacked = split.stacked;
        let id = split.first;
        let share = f32::from(split.ratio) / 1000.;
        let first_pane = self.render_pane(first, active, cx).flex_none().map(|d| {
            if stacked {
                d.w_full().h(relative(share))
            } else {
                d.h_full().w(relative(share))
            }
        });
        let second_pane = self
            .render_pane(second, active, cx)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .map(|d| if stacked { d.w_full() } else { d.h_full() });
        let divider = div()
            .id("split-divider")
            .debug_selector(|| "split-divider".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .bg(colors.surface)
            .hover(|d| d.bg(colors.accent.opacity(0.35)))
            .map(|d| {
                if stacked {
                    d.h(px(6.)).w_full().cursor_row_resize()
                } else {
                    d.w(px(6.)).h_full().cursor_col_resize()
                }
            })
            .child(div().bg(colors.border).map(|d| {
                if stacked {
                    d.h(px(1.)).w_full()
                } else {
                    d.w(px(1.)).h_full()
                }
            }))
            .on_drag(DraggedDivider, |_, _, _, cx| cx.new(|_| gpui::Empty))
            // Double-click for equal halves.
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.layout.set_ratio(id, EVEN_RATIO);
                    cx.notify();
                }
            }));
        div()
            .id("split")
            .debug_selector(|| "split".to_owned())
            .size_full()
            .flex()
            .when(stacked, gpui::Styled::flex_col)
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<DraggedDivider>, _, cx| {
                    let (origin, size) = (event.bounds.origin, event.bounds.size);
                    let at = event.event.position;
                    let share = if stacked {
                        (at.y - origin.y) / size.height
                    } else {
                        (at.x - origin.x) / size.width
                    };
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let ratio = (share.clamp(0., 1.) * 1000.) as u16;
                    this.layout.set_ratio(id, ratio);
                    this.session_dirty = true;
                    cx.notify();
                }),
            )
            .child(first_pane)
            .child(divider)
            .child(second_pane)
            .into_any_element()
    }

    /// One side of a split. Clicking anywhere in it makes it the active note; the other side shows
    /// a small label with its title, like a browser's split view.
    fn render_pane(
        &self,
        note: Entity<NoteEditor>,
        active: NoteId,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let colors = theme(cx);
        let id = note.read(cx).id;
        let title = note.read(cx).display_title();
        let focused = id == active;
        div()
            .id(SharedString::from(format!("pane-{id}")))
            .debug_selector(|| format!("pane-{}", if focused { "focused" } else { "other" }))
            .relative()
            .overflow_hidden()
            .capture_any_mouse_down(cx.listener(move |this, _, window, cx| {
                if this.active_id(cx) != Some(id)
                    && let Some(index) = this.tab_index(id, cx)
                {
                    this.activate(index, window, cx);
                }
            }))
            .child(note)
            .when(focused, |d| {
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(2.))
                        .bg(colors.accent),
                )
            })
            .when(!focused, |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("pane-label-{id}")))
                        .occlude()
                        .absolute()
                        .bottom(px(6.))
                        .right(px(18.))
                        .flex()
                        .items_center()
                        .gap_1()
                        .h(px(24.))
                        .max_w(px(220.))
                        .pl_2()
                        .pr_1()
                        .rounded_md()
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.surface)
                        .text_size(px(12.))
                        .text_color(colors.muted)
                        .cursor_pointer()
                        .child(div().truncate().child(title))
                        .child(
                            div()
                                .id(SharedString::from(format!("pane-close-{id}")))
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(px(18.))
                                .rounded_sm()
                                .hover(|d| d.bg(colors.hover))
                                .child(icon(Icon::X, colors.muted).size(px(12.)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_notes(&[id], window, cx);
                                })),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(index) = this.tab_index(id, cx) {
                                this.activate(index, window, cx);
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event, _, cx| {
                                this.open_tab_menu(id, event, cx);
                            }),
                        ),
                )
            })
    }
}
