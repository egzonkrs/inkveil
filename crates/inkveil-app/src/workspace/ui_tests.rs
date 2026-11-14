//! The tab bar, split view and AI window driven with real mouse events through GPUI's test window,
//! on a throwaway vault in a temporary folder.

use std::time::Duration;

use gpui::{
    Bounds, Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext,
    WeakEntity, point, px,
};
use inkveil_crypto::{KdfParams, Password};
use inkveil_store::{NoteId, Vault, VaultPaths};

use super::{
    ReopenClosedTab, ToggleChat, ToggleSplit, Workspace, ai_panel::AiPanel, chat::ChatPanel,
};
use crate::{ai::AiConfig, theme::Theme};

fn open(cx: &mut TestAppContext) -> (tempfile::TempDir, Entity<Workspace>, &mut VisualTestContext) {
    // Storage answers from its own thread, so waiting for it is not a deadlock.
    cx.executor().allow_parking();
    cx.update(|cx| {
        cx.set_global(Theme::light());
        cx.bind_keys(crate::ui::text_field::key_bindings());
        cx.bind_keys(crate::editor::actions::key_bindings());
        cx.bind_keys(super::key_bindings());
    });
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::create(
        VaultPaths::new(dir.path().join("vault")),
        &Password::new("test password 7"),
        KdfParams::TESTING,
    )
    .unwrap();
    let (workspace, cx) = cx
        .add_window_view(|window, cx| Workspace::new(WeakEntity::new_invalid(), vault, window, cx));
    wait(cx, &workspace, |w, _| w.loaded);
    (dir, workspace, cx)
}

/// Runs the app until `done` is true.
fn wait(
    cx: &mut VisualTestContext,
    workspace: &Entity<Workspace>,
    done: impl Fn(&Workspace, &gpui::App) -> bool,
) {
    for _ in 0..1000 {
        cx.run_until_parked();
        if cx.update(|_, cx| done(workspace.read(cx), cx)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the app did not get there in time");
}

fn new_notes(
    count: usize,
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
) -> Vec<NoteId> {
    for _ in 0..count {
        workspace.update_in(cx, Workspace::new_note);
    }
    cx.run_until_parked();
    order(workspace, cx)
}

fn order(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<NoteId> {
    cx.update(|_, cx| workspace.read(cx).order(cx))
}

fn active(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<NoteId> {
    cx.update(|_, cx| workspace.read(cx).active_id(cx))
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.run_until_parked();
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not on screen"))
}

fn shown(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.run_until_parked();
    cx.debug_bounds(selector).is_some()
}

fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
    let none = Modifiers::none();
    cx.simulate_mouse_move(from, None, none);
    cx.simulate_mouse_down(from, MouseButton::Left, none);
    let halfway = point((from.x + to.x) / 2., (from.y + to.y) / 2.);
    cx.simulate_mouse_move(halfway, MouseButton::Left, none);
    cx.simulate_mouse_move(to, MouseButton::Left, none);
    cx.simulate_mouse_move(to, MouseButton::Left, none);
    cx.simulate_mouse_up(to, MouseButton::Left, none);
    cx.run_until_parked();
}

fn left_part(b: Bounds<Pixels>) -> Point<Pixels> {
    point(b.origin.x + b.size.width * 0.25, b.center().y)
}

fn right_part(b: Bounds<Pixels>) -> Point<Pixels> {
    point(b.origin.x + b.size.width * 0.75, b.center().y)
}

fn right_click(cx: &mut VisualTestContext, at: Point<Pixels>) {
    let none = Modifiers::none();
    cx.simulate_mouse_move(at, None, none);
    cx.simulate_mouse_down(at, MouseButton::Right, none);
    cx.simulate_mouse_up(at, MouseButton::Right, none);
    cx.run_until_parked();
}

fn click(cx: &mut VisualTestContext, at: Point<Pixels>) {
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
}

/// Right-clicks a tab, hovers its first row to open the submenu and clicks `row` in it.
fn arrange(cx: &mut VisualTestContext, tab: &'static str, row: &'static str) {
    let at = bounds(cx, tab).center();
    right_click(cx, at);
    let arrange = bounds(cx, "tab-arrange").center();
    cx.simulate_mouse_move(arrange, None, Modifiers::none());
    let at = bounds(cx, row).center();
    click(cx, at);
}

/// Splits the first two of two open notes through the mouse: drags the first tab onto the right
/// half of the editor, where the second note is showing.
fn split_by_dragging(cx: &mut VisualTestContext) {
    let from = bounds(cx, "tab-0").center();
    let to = right_part(bounds(cx, "single-note"));
    drag(cx, from, to);
}

#[gpui::test]
fn dragging_a_tab_reorders_the_tabs(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let ids = new_notes(3, &workspace, cx);
    let [a, b, c] = ids[..] else { unreachable!() };
    assert_eq!(active(&workspace, cx), Some(c));

    // Onto the right half of the last tab: it becomes the last tab.
    let (from, to) = (
        bounds(cx, "tab-0").center(),
        right_part(bounds(cx, "tab-2")),
    );
    drag(cx, from, to);
    assert_eq!(order(&workspace, cx), [b, c, a]);
    assert_eq!(active(&workspace, cx), Some(c), "the active note stays");

    // Onto the left half of the first tab: it becomes the first tab.
    let (from, to) = (bounds(cx, "tab-2").center(), left_part(bounds(cx, "tab-0")));
    drag(cx, from, to);
    assert_eq!(order(&workspace, cx), [a, b, c]);

    // Onto the empty end of the tab bar.
    let (from, to) = (
        bounds(cx, "tab-0").center(),
        bounds(cx, "tabs-empty").center(),
    );
    drag(cx, from, to);
    assert_eq!(order(&workspace, cx), [b, c, a]);

    // A plain click still selects a tab, and does not move it.
    let at = bounds(cx, "tab-0").center();
    click(cx, at);
    assert_eq!(active(&workspace, cx), Some(b));
    assert_eq!(order(&workspace, cx), [b, c, a]);
}

#[gpui::test]
fn dropping_a_tab_on_the_editor_opens_a_split(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let ids = new_notes(2, &workspace, cx);
    let [a, b] = ids[..] else { unreachable!() };

    split_by_dragging(cx);
    let split = cx.update(|_, cx| workspace.read(cx).layout.split_of(a).copied());
    let split = split.expect("the two notes are split");
    assert_eq!((split.first, split.second), (b, a), "dropped on the right");
    assert_eq!(
        active(&workspace, cx),
        Some(a),
        "the dropped note gets focus"
    );
    assert_eq!(order(&workspace, cx), [b, a]);

    // Both notes are on screen, side by side and about equally wide.
    let (focused, other) = (bounds(cx, "pane-focused"), bounds(cx, "pane-other"));
    assert!(other.origin.x < focused.origin.x);
    assert!((focused.size.width - other.size.width).abs() < px(8.));
    assert_eq!(focused.size.height, other.size.height);

    // Clicking in the other note makes it the active one.
    let at = other.center();
    click(cx, at);
    assert_eq!(active(&workspace, cx), Some(b));
}

#[gpui::test]
fn dropping_on_the_left_puts_the_tab_on_the_left(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let ids = new_notes(2, &workspace, cx);
    let [a, b] = ids[..] else { unreachable!() };
    let (from, to) = (
        bounds(cx, "tab-0").center(),
        left_part(bounds(cx, "single-note")),
    );
    drag(cx, from, to);
    let split = cx.update(|_, cx| workspace.read(cx).layout.split_of(a).copied());
    assert_eq!(split.map(|s| (s.first, s.second)), Some((a, b)));
}

#[gpui::test]
fn the_arrange_menu_does_everything_on_it(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let ids = new_notes(3, &workspace, cx);
    let [a, b, c] = ids[..] else { unreachable!() };
    // c is on screen: drag a next to it, on its right.
    split_by_dragging(cx);
    assert_eq!(order(&workspace, cx), [c, a, b]);

    arrange(cx, "tab-1", "split-swap");
    assert_eq!(order(&workspace, cx), [a, c, b], "swapped");
    let first = |w: &Workspace| w.layout.split_of(a).map(|s| s.first);
    assert_eq!(cx.update(|_, cx| first(workspace.read(cx))), Some(a));

    arrange(cx, "tab-1", "split-orientation");
    let (focused, other) = (bounds(cx, "pane-focused"), bounds(cx, "pane-other"));
    assert!(focused.origin.y != other.origin.y, "one above the other");
    assert_eq!(focused.origin.x, other.origin.x);

    arrange(cx, "tab-1", "split-orientation");
    let (focused, other) = (bounds(cx, "pane-focused"), bounds(cx, "pane-other"));
    assert_eq!(focused.origin.y, other.origin.y, "side by side again");

    arrange(cx, "tab-1", "split-unsplit");
    cx.update(|_, cx| assert_eq!(workspace.read(cx).layout.splits.len(), 0));
    assert_eq!(order(&workspace, cx).len(), 3, "both tabs stay open");
    assert!(shown(cx, "single-note"));
}

#[gpui::test]
fn closing_one_side_keeps_the_other_on_screen(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let ids = new_notes(3, &workspace, cx);
    let [a, b, c] = ids[..] else { unreachable!() };
    split_by_dragging(cx);

    arrange(cx, "tab-1", "split-close-second");
    assert_eq!(order(&workspace, cx), [c, b]);
    assert_eq!(active(&workspace, cx), Some(c));
    cx.update(|_, cx| assert_eq!(workspace.read(cx).layout.splits.len(), 0));

    // It comes back with Ctrl+Shift+T.
    cx.dispatch_action(ReopenClosedTab);
    wait(cx, &workspace, |w, cx| w.tab_index(a, cx).is_some());
    assert_eq!(active(&workspace, cx), Some(a));
}

#[gpui::test]
fn the_shortcut_starts_and_ends_a_split(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let ids = new_notes(1, &workspace, cx);
    // With nothing to pair with, it opens a new note beside this one.
    cx.dispatch_action(ToggleSplit);
    cx.run_until_parked();
    assert_eq!(order(&workspace, cx).len(), 2);
    let split = cx.update(|_, cx| workspace.read(cx).layout.split_of(ids[0]).copied());
    assert!(split.is_some());
    cx.dispatch_action(ToggleSplit);
    cx.run_until_parked();
    cx.update(|_, cx| assert_eq!(workspace.read(cx).layout.splits.len(), 0));
}

#[gpui::test]
fn dragging_the_divider_resizes_the_notes(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(2, &workspace, cx);
    split_by_dragging(cx);
    let split = bounds(cx, "split");
    let from = bounds(cx, "split-divider").center();
    let to = point(split.origin.x + split.size.width * 0.3, from.y);
    drag(cx, from, to);
    let ratio = cx.update(|_, cx| workspace.read(cx).layout.splits[0].ratio);
    assert!((290..=310).contains(&ratio), "ratio is {ratio}");

    // It cannot be dragged so far that a note disappears.
    let (from, to) = (bounds(cx, "split-divider").center(), split.origin);
    drag(cx, from, to);
    let ratio = cx.update(|_, cx| workspace.read(cx).layout.splits[0].ratio);
    assert_eq!(ratio, 200);
}

#[gpui::test]
fn a_tab_in_a_split_drags_its_partner_along(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let ids = new_notes(3, &workspace, cx);
    let [a, b, c] = ids[..] else { unreachable!() };
    split_by_dragging(cx);
    assert_eq!(order(&workspace, cx), [c, a, b]);
    // The pair is tabs 0 and 1. Dragging the second of them to the end brings both.
    let (from, to) = (
        bounds(cx, "tab-1").center(),
        bounds(cx, "tabs-empty").center(),
    );
    drag(cx, from, to);
    assert_eq!(order(&workspace, cx), [b, c, a]);
}

#[gpui::test]
fn pinned_tabs_and_close_commands_work_from_the_menu(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let ids = new_notes(4, &workspace, cx);
    let [a, b, c, d] = ids[..] else {
        unreachable!()
    };

    let at = bounds(cx, "tab-2").center();
    right_click(cx, at);
    let at = bounds(cx, "tab-pin").center();
    click(cx, at);
    assert_eq!(order(&workspace, cx), [c, a, b, d], "pinned tabs go first");

    // An ordinary tab cannot be dragged in front of a pinned one.
    let (from, to) = (bounds(cx, "tab-3").center(), left_part(bounds(cx, "tab-0")));
    drag(cx, from, to);
    assert_eq!(order(&workspace, cx), [c, d, a, b]);

    let at = bounds(cx, "tab-1").center();
    right_click(cx, at);
    let at = bounds(cx, "tab-close-right").center();
    click(cx, at);
    assert_eq!(order(&workspace, cx), [c, d]);

    let at = bounds(cx, "tab-1").center();
    right_click(cx, at);
    let at = bounds(cx, "tab-close-others").center();
    click(cx, at);
    assert_eq!(order(&workspace, cx), [c, d], "pinned tabs stay");
}

#[gpui::test]
fn duplicating_copies_the_text_and_keeps_the_original(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let ids = new_notes(2, &workspace, cx);
    workspace.update(cx, |w, cx| {
        w.tabs[0].update(cx, |note, cx| {
            note.editor
                .update(cx, |e, cx| e.run(cx, |ed| ed.insert_text("hello there")));
        });
    });
    let at = bounds(cx, "tab-0").center();
    right_click(cx, at);
    let at = bounds(cx, "tab-duplicate").center();
    click(cx, at);

    let tabs = order(&workspace, cx);
    assert_eq!(tabs.len(), 3);
    assert_eq!(
        (tabs[0], tabs[2]),
        (ids[0], ids[1]),
        "the copy sits beside it"
    );
    let copy = tabs[1];
    assert_eq!(active(&workspace, cx), Some(copy));
    cx.update(|_, cx| {
        let w = workspace.read(cx);
        let (original, duplicate) = (w.tabs[0].read(cx), w.tabs[1].read(cx));
        assert_eq!(original.doc(cx), duplicate.doc(cx));
        assert!(duplicate.display_title().ends_with("(copy)"));
        assert!(w.notes.iter().any(|n| n.id == copy));
    });
}

#[gpui::test]
fn the_arrangement_survives_locking_and_unlocking(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let ids = new_notes(4, &workspace, cx);
    let [a, b, c, d] = ids[..] else {
        unreachable!()
    };
    workspace.update_in(cx, |w, window, cx| {
        let order = w.layout.toggle_pin(&w.order(cx), d);
        w.apply_order(&order, cx);
        w.split_tabs(a, c, c, window, cx);
        w.layout.toggle_stacked(a);
        w.layout.set_ratio(a, 620);
    });
    assert_eq!(order(&workspace, cx), [d, a, c, b]);
    let layout = cx.update(|_, cx| workspace.read(cx).layout.clone());

    // What is stored, read back the way the app does after unlocking.
    let stored = workspace.update(cx, |w, cx| {
        w.save_session(true, cx);
        w.saved_session.clone().unwrap()
    });
    let stored = super::session::Session::decode(&stored.encode());
    workspace.update_in(cx, |w, window, cx| {
        w.tabs.clear();
        w.active = None;
        w.layout = super::tab_layout::TabLayout::default();
        w.restore_session(stored, window, cx);
    });
    wait(cx, &workspace, |w, _| {
        w.tabs.len() == 4 && w.restoring.splits.is_empty()
    });
    assert_eq!(order(&workspace, cx), [d, a, c, b]);
    cx.update(|_, cx| assert_eq!(workspace.read(cx).layout, layout));
    assert_eq!(active(&workspace, cx), Some(c));
}

fn show_suggestion(workspace: &Entity<Workspace>, index: usize, cx: &mut VisualTestContext) {
    workspace.update(cx, |w, cx| {
        w.tabs[index].update(cx, |note, cx| {
            note.ai = Some(AiPanel::with_suggestion("A better sentence."));
            cx.notify();
        });
    });
    cx.run_until_parked();
}

#[gpui::test]
fn the_ai_window_can_be_dragged_around_the_editor(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(1, &workspace, cx);
    show_suggestion(&workspace, 0, cx);
    let start = bounds(cx, "ai-panel");
    let handle = bounds(cx, "ai-move").center();

    let to = point(handle.x - px(300.), handle.y + px(200.));
    drag(cx, handle, to);
    let moved = bounds(cx, "ai-panel");
    assert_eq!(moved.origin, start.origin + point(px(-300.), px(200.)));
    assert_eq!(moved.size, start.size);

    // The Replace and Discard buttons came along and still work.
    assert!(shown(cx, "ai-move"));

    // It stops at the edges of the editor instead of leaving it.
    let handle = bounds(cx, "ai-move").center();
    drag(cx, handle, point(px(5000.), px(5000.)));
    let corner = bounds(cx, "ai-panel");
    let window = cx.update(|window, _| window.viewport_size());
    assert!(corner.right() <= window.width);
    assert!(corner.bottom() <= window.height);
    let handle = bounds(cx, "ai-move").center();
    drag(cx, handle, point(px(0.), px(0.)));
    let top_left = bounds(cx, "ai-panel");
    assert!(
        top_left.origin.y > px(0.),
        "stays below the tab bar and toolbar"
    );
    let area_left = top_left.origin.x;

    // The next request on this note opens where the window was left.
    workspace.update_in(cx, |w, window, cx| {
        w.tabs[0].update(cx, |note, cx| note.close_ai(window, cx));
    });
    assert!(!shown(cx, "ai-panel"));
    show_suggestion(&workspace, 0, cx);
    assert_eq!(bounds(cx, "ai-panel").origin.x, area_left);
}

#[gpui::test]
fn in_a_split_only_the_dragged_ai_window_moves(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(2, &workspace, cx);
    split_by_dragging(cx);
    show_suggestion(&workspace, 0, cx);
    let handle = bounds(cx, "ai-move").center();
    let to = point(handle.x, handle.y + px(120.));
    drag(cx, handle, to);
    cx.update(|_, cx| {
        let w = workspace.read(cx);
        assert!(w.tabs[0].read(cx).ai_position.is_some());
        assert!(w.tabs[1].read(cx).ai_position.is_none());
    });
    // It fits inside its own half.
    let pane = cx.update(|_, cx| workspace.read(cx).active_id(cx));
    assert!(pane.is_some());
    let panel = bounds(cx, "ai-panel");
    let divider = bounds(cx, "split-divider");
    assert!(panel.right() <= divider.origin.x + px(1.));
}

/// The chat of the workspace, once it has been opened.
fn chat(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Entity<ChatPanel> {
    cx.update(|_, cx| workspace.read(cx).chat.clone())
        .expect("the chat has been opened")
}

fn open_chat(cx: &mut VisualTestContext) {
    let at = bounds(cx, "open-chat").center();
    click(cx, at);
}

fn set_api_key(cx: &mut VisualTestContext) {
    cx.update(|_, cx| {
        cx.set_global(AiConfig {
            api_key: zeroize::Zeroizing::new("test key".to_owned()),
            ..AiConfig::default()
        });
    });
}

#[gpui::test]
fn the_chat_button_swaps_the_note_for_the_chat_and_back(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(1, &workspace, cx);
    assert!(!shown(cx, "chat-panel"));
    let button = bounds(cx, "open-chat");
    assert!(shown(cx, "improve-ai"));

    open_chat(cx);
    assert!(shown(cx, "chat-panel"));
    assert!(shown(cx, "chat-model"), "Grok is shown with its logo");
    assert!(shown(cx, "chat-effort-Low"));
    assert!(shown(cx, "chat-effort-Extra high"));
    assert!(!shown(cx, "improve-ai"), "nothing acts on the hidden note");
    assert_eq!(
        bounds(cx, "open-chat").origin,
        button.origin,
        "the button stays in place, to go back"
    );
    // It fills the note's editor from the toolbar down.
    let panel = bounds(cx, "chat-panel");
    let note = bounds(cx, "single-note");
    assert_eq!(panel.size.width, note.size.width);
    assert_eq!(panel.bottom(), note.bottom());

    open_chat(cx);
    assert!(!shown(cx, "chat-panel"));
    assert!(shown(cx, "improve-ai"));
}

#[gpui::test]
fn the_chat_works_with_no_note_open_and_with_the_keyboard(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    cx.dispatch_action(ToggleChat);
    assert!(shown(cx, "chat-panel"));
    cx.dispatch_action(ToggleChat);
    assert!(!shown(cx, "chat-panel"));

    new_notes(1, &workspace, cx);
    cx.dispatch_action(ToggleChat);
    assert!(shown(cx, "chat-panel"));
    // Escape goes back to the note; the text field has the focus.
    cx.simulate_keystrokes("escape");
    assert!(!shown(cx, "chat-panel"));
}

#[gpui::test]
fn one_conversation_follows_you_between_notes_until_the_vault_locks(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(2, &workspace, cx);
    open_chat(cx);
    let panel = chat(&workspace, cx);
    panel.update(cx, |p, _| p.add_turn(true, "first question"));

    // The first tab shows its note; opening the chat there brings the conversation along.
    let at = bounds(cx, "tab-0").center();
    click(cx, at);
    assert!(!shown(cx, "chat-panel"));
    open_chat(cx);
    assert!(shown(cx, "chat-panel"));
    let count = chat(&workspace, cx).read_with(cx, |p, _| p.turn_count());
    assert_eq!(count, 1);
    cx.update(|_, cx| {
        let open = workspace.read(cx).tabs.iter();
        assert_eq!(open.filter(|t| t.read(cx).chat_open()).count(), 1);
    });

    // Locking forgets it, and nothing keeps it alive.
    let weak = panel.downgrade();
    drop(panel);
    workspace.update(cx, |w, cx| {
        w.prepare_lock(cx);
    });
    cx.run_until_parked();
    assert!(!shown(cx, "chat-panel"));
    assert!(weak.upgrade().is_none(), "the conversation is gone");
}

#[gpui::test]
fn thinking_levels_can_be_picked_and_replies_show_their_time(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(1, &workspace, cx);
    open_chat(cx);
    let panel = chat(&workspace, cx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.effort()),
        crate::ai::Effort::Low
    );
    let at = bounds(cx, "chat-effort-High").center();
    click(cx, at);
    assert_eq!(
        panel.read_with(cx, |p, _| p.effort()),
        crate::ai::Effort::High
    );

    panel.update(cx, |p, cx| {
        p.add_turn(true, "question");
        p.add_turn(false, "**Answer** with `code`:\n\n```\nlet a = 1;\n```");
        p.add_stats(Duration::from_millis(3400), Duration::from_millis(5100));
        cx.notify();
    });
    assert!(shown(cx, "chat-stats-1"));
}

#[gpui::test]
fn a_question_with_a_secret_waits_for_a_decision(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(1, &workspace, cx);
    set_api_key(cx);
    open_chat(cx);
    let panel = chat(&workspace, cx);
    let secret = "my key is sk-or-v1-0123456789abcdef0123456789abcdef";
    panel.update(cx, |p, cx| {
        p.set_question(secret, cx);
        p.send(cx);
    });
    assert!(shown(cx, "chat-secrets"));
    panel.read_with(cx, |p, cx| {
        assert!(p.asks_about_secrets());
        assert_eq!(p.turn_count(), 0, "nothing was sent");
        assert_eq!(p.question(cx), secret, "and the text is still in the field");
    });
    // Editing the question drops the warning.
    panel.update(cx, |p, cx| {
        cx.notify();
        p.set_question("harmless", cx);
    });
}

#[gpui::test]
fn without_a_key_the_chat_asks_for_one_and_sends_nothing(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(1, &workspace, cx);
    open_chat(cx);
    let panel = chat(&workspace, cx);
    panel.update(cx, |p, cx| {
        p.set_question("hello", cx);
        p.send(cx);
    });
    cx.run_until_parked();
    assert_eq!(panel.read_with(cx, |p, _| p.turn_count()), 0);
    let opened = cx.update(|_, cx| workspace.read(cx).ai_dialog.is_some());
    assert!(opened, "the AI settings open");
}

#[gpui::test]
fn stopping_keeps_what_was_written_or_gives_the_question_back(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(1, &workspace, cx);
    open_chat(cx);
    let panel = chat(&workspace, cx);

    panel.update(cx, |p, cx| {
        p.add_turn(true, "first");
        p.pretend_working("half of the answer");
        cx.notify();
    });
    assert!(shown(cx, "chat-progress"));
    panel.update(cx, super::chat::ChatPanel::press_stop);
    assert_eq!(panel.read_with(cx, |p, _| p.turn_count()), 2);

    panel.update(cx, |p, cx| {
        p.add_turn(true, "second");
        p.pretend_working("");
        p.press_stop(cx);
    });
    panel.read_with(cx, |p, cx| {
        assert_eq!(p.turn_count(), 2);
        assert_eq!(p.question(cx), "second");
    });
}

/// A chat with one question and one reply, on screen.
fn chat_with_reply(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Entity<ChatPanel> {
    new_notes(1, workspace, cx);
    open_chat(cx);
    let panel = chat(workspace, cx);
    panel.update(cx, |p, cx| {
        p.add_turn(true, "what is a stash");
        p.add_turn(false, "A place for changes.\n\nThey wait there.");
        cx.notify();
    });
    cx.run_until_parked();
    panel
}

fn selected(panel: &Entity<ChatPanel>, cx: &mut VisualTestContext) -> Option<String> {
    panel.read_with(cx, |p, _| p.selection_text())
}

#[gpui::test]
fn dragging_selects_text_in_a_reply_and_in_a_question(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let panel = chat_with_reply(&workspace, cx);

    let reply = bounds(cx, "chat-text-1");
    let from = point(reply.origin.x + px(1.), reply.center().y);
    drag(cx, from, point(reply.right(), reply.center().y));
    assert_eq!(
        selected(&panel, cx).as_deref(),
        Some("A place for changes.")
    );

    let question = bounds(cx, "chat-text-0");
    let from = point(question.origin.x + px(1.), question.center().y);
    drag(cx, from, point(question.right(), question.center().y));
    assert_eq!(selected(&panel, cx).as_deref(), Some("what is a stash"));

    // A plain click drops the selection.
    let at = bounds(cx, "chat-text-1").center();
    click(cx, at);
    assert_eq!(selected(&panel, cx), None);
}

#[gpui::test]
fn a_selection_can_run_across_messages_in_either_direction(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let panel = chat_with_reply(&workspace, cx);
    let (top, bottom) = (bounds(cx, "chat-text-0"), bounds(cx, "chat-text-2"));
    let start = point(top.origin.x + px(1.), top.center().y);
    let end = point(bottom.right(), bottom.center().y);
    let whole = "what is a stash\n\nA place for changes.\n\nThey wait there.";

    drag(cx, start, end);
    assert_eq!(selected(&panel, cx).as_deref(), Some(whole));
    drag(cx, end, start);
    assert_eq!(selected(&panel, cx).as_deref(), Some(whole));
}

#[gpui::test]
fn the_keyboard_selects_copies_and_goes_back_to_typing(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let panel = chat_with_reply(&workspace, cx);
    let reply = bounds(cx, "chat-text-1");
    let from = point(reply.origin.x + px(1.), reply.center().y);
    drag(cx, from, point(reply.right(), reply.center().y));

    cx.simulate_keystrokes("ctrl-c");
    let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("A place for changes."));

    cx.simulate_keystrokes("ctrl-a");
    let all = selected(&panel, cx).unwrap_or_default();
    assert!(all.starts_with("what is") && all.ends_with("wait there."));

    // Typing goes to the question, without clicking it first.
    cx.simulate_input("h");
    cx.simulate_input("i");
    assert_eq!(panel.read_with(cx, ChatPanel::question), "hi");
}

#[gpui::test]
fn the_question_being_typed_can_be_selected(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(1, &workspace, cx);
    open_chat(cx);
    let panel = chat(&workspace, cx);
    panel.update(cx, |p, cx| p.set_question("select this text", cx));
    cx.run_until_parked();
    let input = bounds(cx, "chat-input");
    drag(
        cx,
        point(input.origin.x, input.center().y),
        point(input.right(), input.center().y),
    );
    let picked = panel.read_with(cx, ChatPanel::field_selection);
    assert_eq!(picked, "select this text");
}

#[gpui::test]
fn the_cost_panel_shows_beside_the_chat_and_can_be_hidden(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let panel = chat_with_reply(&workspace, cx);
    panel.update(cx, |p, cx| {
        p.add_cost(0.0012);
        cx.notify();
    });
    assert!(shown(cx, "chat-cost-total"));
    let (costs, messages) = (bounds(cx, "chat-costs"), bounds(cx, "chat-messages"));
    assert!(costs.origin.x >= messages.right(), "it is on the right");

    let at = bounds(cx, "chat-show-costs").center();
    click(cx, at);
    assert!(!shown(cx, "chat-costs"));
}

const LONG_QUESTION: &str = "How do I undo my last commit but keep the changes, and what is \
    the difference between reset, revert and restore? Please explain it step by step so that I \
    can follow along in my own repository without losing any work on the way there.";

/// An open chat with the keyboard in its question field.
fn empty_chat(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Entity<ChatPanel> {
    new_notes(1, workspace, cx);
    open_chat(cx);
    chat(workspace, cx)
}

#[gpui::test]
fn a_long_question_wraps_and_the_field_grows_with_it(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let panel = empty_chat(&workspace, cx);
    let one_line = bounds(cx, "chat-input");

    panel.update(cx, |p, cx| p.set_question(LONG_QUESTION, cx));
    let wrapped = bounds(cx, "chat-input");
    assert!(
        wrapped.size.height >= one_line.size.height * 2.,
        "the text stacks into lines instead of running off to the side"
    );
    assert_eq!(wrapped.size.width, one_line.size.width);
    assert_eq!(wrapped.bottom(), one_line.bottom(), "it grows upwards");

    // A very long question stops growing and scrolls instead of pushing the chat away.
    panel.update(cx, |p, cx| p.set_question(&LONG_QUESTION.repeat(12), cx));
    let full = bounds(cx, "chat-input");
    assert_eq!(full.size.height, one_line.size.height * 8.);
    assert!(bounds(cx, "chat-send").bottom() <= bounds(cx, "chat-panel").bottom());

    panel.update(cx, |p, cx| p.set_question("", cx));
    assert_eq!(bounds(cx, "chat-input"), one_line);
}

#[gpui::test]
fn shift_enter_starts_a_new_line_and_the_arrows_move_between_lines(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let panel = empty_chat(&workspace, cx);
    cx.simulate_input("first");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("second");
    assert_eq!(panel.read_with(cx, ChatPanel::question), "first\nsecond");
    let two_lines = bounds(cx, "chat-input").size.height;

    // Up goes to the first line, and typing lands there.
    cx.simulate_keystrokes("up home");
    cx.simulate_input("the ");
    assert_eq!(
        panel.read_with(cx, ChatPanel::question),
        "the first\nsecond"
    );
    // Down then End is the end of the second line.
    cx.simulate_keystrokes("down end");
    cx.simulate_input("!");
    assert_eq!(
        panel.read_with(cx, ChatPanel::question),
        "the first\nsecond!"
    );
    // Shift+Up selects back to the same column of the line above.
    cx.simulate_keystrokes("shift-up");
    let picked = panel.read_with(cx, ChatPanel::field_selection);
    assert_eq!(picked, "st\nsecond!");

    // Ctrl+Z takes typing back a step at a time; Ctrl+Y brings it again.
    cx.simulate_keystrokes("ctrl-z");
    assert_eq!(
        panel.read_with(cx, ChatPanel::question),
        "the first\nsecond"
    );
    cx.simulate_keystrokes("ctrl-y");
    assert_eq!(
        panel.read_with(cx, ChatPanel::question),
        "the first\nsecond!"
    );
    assert_eq!(bounds(cx, "chat-input").size.height, two_lines);

    // A pasted text keeps its lines.
    cx.update(|_, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("one\r\ntwo".to_owned()));
    });
    cx.simulate_keystrokes("ctrl-a ctrl-v");
    assert_eq!(panel.read_with(cx, ChatPanel::question), "one\ntwo");
}

#[gpui::test]
fn a_drag_in_the_question_goes_on_selecting_outside_the_field(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let panel = empty_chat(&workspace, cx);
    panel.update(cx, |p, cx| p.set_question("select this text", cx));
    let input = bounds(cx, "chat-input");
    let none = Modifiers::none();

    // From the end of the text to far above and left of the field: everything before it.
    let from = point(input.right() - px(2.), input.center().y);
    cx.simulate_mouse_move(from, None, none);
    cx.simulate_mouse_down(from, MouseButton::Left, none);
    let outside = point(input.origin.x - px(40.), input.origin.y - px(200.));
    cx.simulate_mouse_move(outside, MouseButton::Left, none);
    cx.run_until_parked();
    let picked = panel.read_with(cx, ChatPanel::field_selection);
    assert_eq!(picked, "select this text");
    cx.simulate_mouse_up(outside, MouseButton::Left, none);

    // Moving the mouse afterwards changes nothing.
    cx.simulate_mouse_move(input.center(), None, none);
    cx.run_until_parked();
    let picked = panel.read_with(cx, ChatPanel::field_selection);
    assert_eq!(picked, "select this text");
}

#[gpui::test]
fn a_double_click_in_the_question_selects_a_word_and_a_triple_click_the_line(
    cx: &mut TestAppContext,
) {
    let (_dir, workspace, cx) = open(cx);
    let panel = empty_chat(&workspace, cx);
    panel.update(cx, |p, cx| p.set_question("select this text\nand more", cx));
    let input = bounds(cx, "chat-input");
    // On the first of its two lines, a little way in: inside "select".
    let at = point(input.origin.x + px(12.), input.origin.y + px(4.));
    let press = |count: usize| gpui::MouseDownEvent {
        button: MouseButton::Left,
        position: at,
        modifiers: Modifiers::none(),
        click_count: count,
        first_mouse: false,
    };
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.simulate_event(press(2));
    // A hand that moves a little during the double click does not undo it.
    cx.simulate_mouse_move(
        at + point(px(30.), px(0.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.run_until_parked();
    let picked = panel.read_with(cx, ChatPanel::field_selection);
    assert_eq!(picked, "select");

    cx.simulate_event(press(3));
    cx.run_until_parked();
    let picked = panel.read_with(cx, ChatPanel::field_selection);
    assert_eq!(picked, "select this text");
}

fn find_open(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| {
        let w = workspace.read(cx);
        w.active.is_some_and(|a| w.tabs[a].read(cx).find_open())
    })
}

#[gpui::test]
fn escape_closes_the_menu_on_top_and_then_what_is_under_it(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(2, &workspace, cx);
    cx.simulate_keystrokes("ctrl-f");
    assert!(find_open(&workspace, cx));

    // A tab's menu over the find bar: Escape closes the menu only.
    let at = bounds(cx, "tab-0").center();
    right_click(cx, at);
    assert!(shown(cx, "tab-pin"));
    cx.simulate_keystrokes("escape");
    assert!(!shown(cx, "tab-pin"));
    assert!(find_open(&workspace, cx), "the find bar is still there");

    // The next Escape closes the find bar.
    cx.simulate_keystrokes("escape");
    assert!(!find_open(&workspace, cx));
}

#[gpui::test]
fn the_ai_settings_close_with_escape_save_with_enter_and_give_the_keyboard_back(
    cx: &mut TestAppContext,
) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(1, &workspace, cx);
    let before = cx.update(|window, cx| window.focused(cx));
    assert!(before.is_some());

    workspace.update_in(cx, |w, window, cx| w.open_ai_settings(None, window, cx));
    cx.run_until_parked();
    let during = cx.update(|window, cx| window.focused(cx));
    assert!(during != before, "the key field has the keyboard");
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        assert!(workspace.read(cx).ai_dialog.is_none());
        assert!(window.focused(cx) == before, "typing goes on where it was");
        assert!(AiConfig::get(cx).is_none(), "nothing was saved");
    });

    // Typing a key and pressing Enter saves it.
    workspace.update_in(cx, |w, window, cx| w.open_ai_settings(None, window, cx));
    cx.run_until_parked();
    cx.simulate_input("test key");
    cx.simulate_keystrokes("enter");
    cx.update(|window, cx| {
        assert!(workspace.read(cx).ai_dialog.is_none());
        assert_eq!(window.focused(cx), before);
        assert!(AiConfig::get(cx).is_some());
    });
}

#[gpui::test]
fn closing_a_tab_right_after_typing_updates_its_row_in_the_sidebar(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let ids = new_notes(1, &workspace, cx);
    workspace.update(cx, |w, cx| {
        w.tabs[0].update(cx, |note, cx| {
            note.editor
                .update(cx, |e, cx| e.run(cx, |ed| ed.insert_text("shopping list")));
        });
    });
    cx.dispatch_action(super::CloseTab);
    cx.run_until_parked();
    cx.update(|_, cx| {
        let w = workspace.read(cx);
        assert_eq!(w.tabs.len(), 0);
        let row = w.notes.iter().find(|n| n.id == ids[0]).unwrap();
        assert!(
            row.meta.preview.contains("shopping list") || row.meta.title.contains("shopping list"),
            "the sidebar shows what was typed: {:?} / {:?}",
            row.meta.title,
            row.meta.preview
        );
    });
}

#[gpui::test]
fn clicking_a_menu_button_again_closes_its_menu(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(1, &workspace, cx);

    // The settings menu from the sidebar's gear.
    let gear = bounds(cx, "open-settings").center();
    click(cx, gear);
    assert!(shown(cx, "settings-menu"));
    click(cx, gear);
    assert!(!shown(cx, "settings-menu"), "the second click closes it");
    click(cx, gear);
    assert!(shown(cx, "settings-menu"), "and the third opens it again");
    // A click elsewhere closes it, and the gear still opens it afterwards.
    let elsewhere = bounds(cx, "single-note").center();
    click(cx, elsewhere);
    assert!(!shown(cx, "settings-menu"));
    click(cx, gear);
    assert!(shown(cx, "settings-menu"));
    click(cx, elsewhere);

    // A toolbar menu of the note: the link popup.
    let link = bounds(cx, "link-button").center();
    click(cx, link);
    assert!(shown(cx, "toolbar-menu"));
    click(cx, link);
    assert!(!shown(cx, "toolbar-menu"), "the second click closes it");
    click(cx, link);
    assert!(shown(cx, "toolbar-menu"));
}

#[gpui::test]
fn the_chat_scrollbar_can_be_dragged_and_does_not_select_text(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let panel = empty_chat(&workspace, cx);
    panel.update(cx, |p, cx| {
        for _ in 0..40 {
            p.add_turn(true, "a question");
            p.add_turn(false, "An answer.\n\nWith a second paragraph.");
        }
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let (at, furthest) = panel.read_with(cx, |p, _| p.scrolled());
    assert!(furthest > px(200.), "there is something to scroll");
    assert_eq!(at, furthest, "a new conversation shows its end");

    // Take the thumb at the bottom of the bar and pull it to the top.
    let messages = bounds(cx, "chat-messages");
    let x = messages.right() - px(6.);
    drag(
        cx,
        point(x, messages.bottom() - px(6.)),
        point(x, messages.origin.y - px(50.)),
    );
    let (at, _) = panel.read_with(cx, |p, _| p.scrolled());
    assert_eq!(at, px(0.), "the start of the conversation is on screen");
    assert!(!panel.read_with(cx, |p, _| p.follows_the_end()));
    assert_eq!(selected(&panel, cx), None, "the drag selected no text");

    // A click low on the bar jumps most of the way down; dragging to the end follows again.
    click(cx, point(x, messages.bottom() - px(40.)));
    let (at, furthest) = panel.read_with(cx, |p, _| p.scrolled());
    assert!(at > furthest * 0.5);
    drag(
        cx,
        point(x, messages.bottom() - px(40.)),
        point(x, messages.bottom() + px(80.)),
    );
    let (at, furthest) = panel.read_with(cx, |p, _| p.scrolled());
    assert_eq!(at, furthest);
    assert!(panel.read_with(cx, |p, _| p.follows_the_end()));
}

#[gpui::test]
fn a_right_click_in_a_text_field_opens_its_menu(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    let panel = empty_chat(&workspace, cx);
    panel.update(cx, |p, cx| p.set_question("copy me", cx));
    cx.simulate_keystrokes("ctrl-a");
    let input = bounds(cx, "chat-input");
    right_click(cx, point(input.origin.x + px(10.), input.center().y));
    assert!(shown(cx, "field-paste"));

    // Copy from the menu copies the selection and closes the menu.
    let at = bounds(cx, "field-copy").center();
    click(cx, at);
    assert!(!shown(cx, "field-paste"));
    let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("copy me"));

    // Escape closes the menu and nothing else: the chat stays open.
    right_click(cx, point(input.origin.x + px(10.), input.center().y));
    assert!(shown(cx, "field-paste"));
    cx.simulate_keystrokes("escape");
    assert!(!shown(cx, "field-paste"));
    assert!(shown(cx, "chat-panel"));

    // Select all, then Cut, from the menu empties the field.
    right_click(cx, point(input.origin.x + px(10.), input.center().y));
    let at = bounds(cx, "field-select-all").center();
    click(cx, at);
    right_click(cx, point(input.origin.x + px(10.), input.center().y));
    let at = bounds(cx, "field-cut").center();
    click(cx, at);
    assert_eq!(panel.read_with(cx, ChatPanel::question), "");
}

fn note_text(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| {
        let w = workspace.read(cx);
        w.tabs[w.active.unwrap()].read(cx).doc(cx).plain_text()
    })
}

#[gpui::test]
fn typing_with_a_menu_open_does_not_change_the_note_behind_it(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(1, &workspace, cx);
    workspace.update_in(cx, |w, window, cx| {
        w.tabs[0].update(cx, |note, cx| {
            note.editor.update(cx, |e, cx| e.focus(window, cx));
        });
    });
    cx.simulate_keystrokes("a b");
    assert_eq!(note_text(&workspace, cx), "ab");

    let at = bounds(cx, "tab-0").center();
    right_click(cx, at);
    assert!(shown(cx, "tab-pin"));
    cx.simulate_keystrokes("x y ctrl-n enter");
    assert_eq!(order(&workspace, cx).len(), 1, "Ctrl+N made no note");
    cx.simulate_keystrokes("escape");
    assert_eq!(note_text(&workspace, cx), "ab", "nothing was typed into it");

    // With the menu closed, typing works again.
    cx.simulate_keystrokes("c");
    assert_eq!(note_text(&workspace, cx), "abc");
}

#[gpui::test]
fn the_ai_settings_keep_the_keyboard_to_themselves(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = open(cx);
    new_notes(1, &workspace, cx);
    workspace.update_in(cx, |w, window, cx| w.open_ai_settings(None, window, cx));
    cx.run_until_parked();
    let key_field = cx.update(|window, cx| window.focused(cx));

    // Workspace shortcuts do nothing under the dialog.
    cx.simulate_keystrokes("ctrl-n ctrl-w ctrl-j");
    assert_eq!(order(&workspace, cx).len(), 1);
    assert!(!shown(cx, "chat-panel"));
    cx.update(|_, cx| assert!(workspace.read(cx).ai_dialog.is_some()));

    // Tab moves between its two fields and never out of it.
    cx.simulate_keystrokes("tab");
    let model_field = cx.update(|window, cx| window.focused(cx));
    assert_ne!(model_field, key_field);
    cx.simulate_keystrokes("tab");
    assert_eq!(cx.update(|window, cx| window.focused(cx)), key_field);

    // Typing and the clipboard keys still work in its fields.
    cx.simulate_keystrokes("a b ctrl-a ctrl-x");
    cx.simulate_keystrokes("escape");
    cx.update(|_, cx| assert!(workspace.read(cx).ai_dialog.is_none()));
}
