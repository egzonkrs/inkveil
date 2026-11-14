//! The mini chat: quick questions to Grok, filling the editor of the open note.
//!
//! Privacy: the conversation exists only in this panel's memory. It is never written to the vault
//! or anywhere else, it is wiped when the vault locks or the app closes, and every question goes
//! through the rules in [`crate::ai`] (zero data retention, secret check first).

use std::{
    cell::RefCell,
    collections::HashMap,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Context, CursorStyle, Entity, EventEmitter,
    FocusHandle, Focusable, HighlightStyle, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, Point, Render, ScrollWheelEvent, SharedString, StyledText, Task,
    TextLayout, Window, actions, combine_highlights, div, point, prelude::*, pulsating_between, px,
    relative,
};
use zeroize::Zeroizing;

use super::{
    Workspace,
    chat_selection::{TextPos, TextSelection, word_at},
    chat_text::{self, Block, Inline, Mark},
};
use crate::{
    ai::{self, AiConfig, ChatTurn, Effort, Progress},
    assets::Icon,
    secrets::Finding,
    theme::{MONO_FONT, Theme, theme},
    ui::{
        Tooltip, button, hairline, icon, icon_button,
        scrollbar::Scrollbar,
        text_field::{TextField, TextFieldEvent},
    },
};

actions!(chat, [CloseChat, CopySelection, SelectAllText]);

const CONTEXT: &str = "MiniChat";
/// Widest the conversation gets, so lines stay easy to read in a big window.
const COLUMN_WIDTH: f32 = 760.;
/// Width of the cost panel.
const COSTS_WIDTH: f32 = 232.;
/// How close to the edge of the conversation a selection drag starts scrolling it.
const DRAG_SCROLL_EDGE: f32 = 16.;
/// How far one mouse movement scrolls during such a drag.
const DRAG_SCROLL_STEP: f32 = 24.;
/// How often a running request is checked and more of the reply is shown: about 30 times a second.
const FRAME: Duration = Duration::from_millis(33);
/// The thinking level a new chat starts with: quick, for small questions.
const DEFAULT_EFFORT: Effort = Effort::Low;
/// How close to the end of the conversation counts as being at the end.
const NEAR_END: f32 = 32.;
/// How many lines the question field shows before it scrolls.
const COMPOSER_LINES: usize = 8;
/// How long a copy button shows its check mark.
const COPIED_FOR: Duration = Duration::from_millis(1500);

pub fn key_bindings() -> Vec<gpui::KeyBinding> {
    let ctx = Some(CONTEXT);
    vec![
        gpui::KeyBinding::new("escape", CloseChat, ctx),
        gpui::KeyBinding::new("ctrl-c", CopySelection, ctx),
        gpui::KeyBinding::new("ctrl-a", SelectAllText, ctx),
    ]
}

/// What the panel asks the workspace to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatEvent {
    Close,
    OpenSettings,
}

/// What the model is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Waiting,
    Thinking,
    Writing,
    /// Pausing before the next retry.
    Pause {
        reason: String,
        until: Instant,
    },
}

/// How long a reply took and what it cost.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ReplyStats {
    /// From sending the question to the first word of the reply.
    thought: Duration,
    /// From sending the question to the end of the reply.
    total: Duration,
    effort: Effort,
    usage: Option<ai::Usage>,
}

/// One request in the cost panel.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RequestCost {
    effort: Effort,
    /// `None` for a request that was stopped: OpenRouter reports usage at the very end.
    usage: Option<ai::Usage>,
}

impl RequestCost {
    fn cost(&self) -> Option<f64> {
        self.usage.and_then(|usage| usage.cost)
    }
}

/// What the requests cost, for as long as the vault stays unlocked.
#[derive(Debug, Default)]
struct CostLog {
    /// The requests of this chat.
    requests: Vec<RequestCost>,
    /// The total of earlier chats since the vault was unlocked.
    earlier: f64,
}

impl CostLog {
    fn add(&mut self, effort: Effort, usage: Option<ai::Usage>) {
        self.requests.push(RequestCost { effort, usage });
    }

    fn chat_total(&self) -> f64 {
        self.requests.iter().filter_map(RequestCost::cost).sum()
    }

    fn session_total(&self) -> f64 {
        self.earlier + self.chat_total()
    }

    /// A new chat starts at zero; what was spent still counts since unlocking.
    fn start_new_chat(&mut self) {
        self.earlier = self.session_total();
        self.requests.clear();
    }

    /// Whether a request's cost is missing, so the real total is higher.
    fn has_unknown(&self) -> bool {
        self.requests.iter().any(|r| r.cost().is_none())
    }
}

/// An amount of money: "$0.0012", "$1.25", or "<$0.0001" for what would round to nothing.
fn cost_label(cost: f64) -> String {
    if cost <= 0. {
        "$0".to_owned()
    } else if cost < 0.0001 {
        "<$0.0001".to_owned()
    } else if cost < 1. {
        format!("${cost:.4}")
    } else {
        format!("${cost:.2}")
    }
}

/// A time in words: "3.4 s", "12 s" or "2 min 5 s".
fn time_label(time: Duration) -> String {
    let seconds = time.as_secs();
    if seconds >= 60 {
        format!("{} min {} s", seconds / 60, seconds % 60)
    } else if seconds >= 10 {
        format!("{seconds} s")
    } else {
        format!("{:.1} s", time.as_secs_f32())
    }
}

/// A request on its way.
struct Working {
    started: Instant,
    /// When the latest attempt started. A retry thinks from the beginning.
    attempt_started: Instant,
    /// The retry under way (1 to [`ai::MAX_RETRIES`]), or 0 for the first attempt.
    retry: u32,
    phase: Phase,
    effort: Effort,
    /// How long the model took to start writing, once it has.
    thought: Option<Duration>,
    /// What the request used, once OpenRouter has said.
    usage: Option<ai::Usage>,
    /// The reply received so far.
    received: Zeroizing<String>,
    /// How much of it is on screen, in bytes. Text is revealed evenly, however it arrives.
    shown: usize,
    /// The whole reply, once it is complete. It goes into the chat when all of it is on screen.
    finished: Option<Zeroizing<String>>,
}

impl Working {
    fn new(effort: Effort) -> Self {
        let now = Instant::now();
        Self {
            started: now,
            attempt_started: now,
            retry: 0,
            phase: Phase::Waiting,
            effort,
            thought: None,
            usage: None,
            received: Zeroizing::default(),
            shown: 0,
            finished: None,
        }
    }

    fn apply(&mut self, progress: Progress) {
        match progress {
            Progress::Waiting { attempt } => {
                self.retry = attempt.saturating_sub(1);
                self.phase = Phase::Waiting;
                // A new attempt writes its reply from the start.
                self.attempt_started = Instant::now();
                self.thought = None;
                self.usage = None;
                self.received.clear();
                self.shown = 0;
            }
            Progress::Thinking => self.phase = Phase::Thinking,
            Progress::Writing { .. } => self.phase = Phase::Writing,
            Progress::Piece(text) => {
                self.thought
                    .get_or_insert_with(|| self.attempt_started.elapsed());
                self.received.push_str(&text);
            }
            Progress::Usage(usage) => self.usage = Some(usage),
            Progress::Retrying {
                retry,
                reason,
                wait,
            } => {
                self.retry = retry;
                self.phase = Phase::Pause {
                    reason,
                    until: Instant::now() + wait,
                };
            }
        }
    }

    /// Shows a little more of the reply. Returns whether anything changed.
    fn reveal(&mut self) -> bool {
        let waiting = &self.received[self.shown..];
        if waiting.is_empty() {
            return false;
        }
        let step = reveal_step(waiting.chars().count());
        self.shown += waiting
            .char_indices()
            .nth(step)
            .map_or(waiting.len(), |(at, _)| at);
        true
    }

    fn fully_shown(&self) -> bool {
        self.shown == self.received.len()
    }

    /// How long the reply took, if it has started.
    fn stats(&self) -> Option<ReplyStats> {
        Some(ReplyStats {
            thought: self.thought?,
            total: self.started.elapsed(),
            effort: self.effort,
            usage: self.usage,
        })
    }

    /// Changes every tenth of a second, which is how finely the time is shown.
    fn tick(&self) -> u128 {
        self.attempt_started.elapsed().as_millis() / 100
    }
}

/// How many of the `waiting` characters to show in this frame: a steady trickle that speeds up
/// when the model writes faster than the text is shown.
fn reveal_step(waiting: usize) -> usize {
    (waiting / 6).max(3)
}

/// Where the panel is, besides showing the conversation.
enum Status {
    Idle,
    /// The question looks like it contains secrets; it stays in the field until the user decides.
    ConfirmSecrets(Vec<Finding>),
    Working(Working),
    /// The last question got no answer. It can be asked again.
    Failed(String),
    /// Something the user can fix before sending; nothing was sent.
    Notice(&'static str),
}

/// Which copy button was just clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyTarget {
    Reply(usize),
    Code(usize, usize),
}

/// A piece of text on screen that can be selected.
struct TextBlock {
    text: SharedString,
    layout: TextLayout,
}

/// The chat with Grok.
pub struct ChatPanel {
    field: Entity<TextField>,
    /// Has the keyboard while text in the conversation is selected, so Ctrl+C copies that.
    text_focus: FocusHandle,
    turns: Vec<ChatTurn>,
    /// How long each reply took, by its place in `turns`.
    stats: HashMap<usize, ReplyStats>,
    costs: CostLog,
    show_costs: bool,
    /// The text blocks of the last frame, from the top down.
    blocks: RefCell<Vec<TextBlock>>,
    selection: Option<TextSelection>,
    /// The mouse is dragging a selection.
    selecting: bool,
    effort: Effort,
    status: Status,
    scroll: Scrollbar,
    /// Keep the end of the conversation in view; off once the user scrolls up to read.
    follow: bool,
    copied: Option<CopyTarget>,
    /// Dropping it stops the request.
    request: Option<Task<()>>,
    copied_reset: Option<Task<()>>,
}

impl EventEmitter<ChatEvent> for ChatPanel {}

impl Focusable for ChatPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.field.focus_handle(cx)
    }
}

impl ChatPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = cx.new(|cx| {
            TextField::new("Ask Grok anything", window, cx)
                .borderless()
                .multiline(COMPOSER_LINES)
        });
        cx.subscribe(&field, |this, _, event, cx| match event {
            TextFieldEvent::Submitted => this.send(cx),
            TextFieldEvent::Changed => {
                if matches!(this.status, Status::ConfirmSecrets(_) | Status::Notice(_)) {
                    this.status = Status::Idle;
                }
                this.selection = None;
                cx.notify();
            }
            TextFieldEvent::Blurred => cx.notify(),
        })
        .detach();
        Self {
            field,
            text_focus: cx.focus_handle(),
            turns: Vec::new(),
            stats: HashMap::new(),
            costs: CostLog::default(),
            show_costs: true,
            blocks: RefCell::default(),
            selection: None,
            selecting: false,
            effort: DEFAULT_EFFORT,
            status: Status::Idle,
            scroll: Scrollbar::new(),
            follow: true,
            copied: None,
            request: None,
            copied_reset: None,
        }
    }

    fn busy(&self) -> bool {
        matches!(self.status, Status::Working(_))
    }

    /// Sends what is in the field, after the same checks as Improve and Humanize.
    pub(super) fn send(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let question = Zeroizing::new(self.field.read(cx).text().trim().to_owned());
        if question.is_empty() {
            return;
        }
        if question.chars().count() > ai::MAX_CHARS {
            self.status = Status::Notice("That is too long. Send up to about 8,000 characters.");
            cx.notify();
            return;
        }
        if AiConfig::get(cx).is_none() {
            cx.emit(ChatEvent::OpenSettings);
            return;
        }
        // Check for keys, passwords and other secrets before anything leaves the computer.
        let findings = crate::secrets::scan(&question);
        if !findings.is_empty() {
            self.status = Status::ConfirmSecrets(findings);
            self.follow = true;
            cx.notify();
            return;
        }
        self.send_unchecked(cx);
    }

    /// Moves the question from the field into the chat and sends it.
    fn send_unchecked(&mut self, cx: &mut Context<Self>) {
        let question = Zeroizing::new(self.field.read(cx).text().trim().to_owned());
        self.field.update(cx, TextField::clear);
        self.turns.push(ChatTurn {
            from_user: true,
            text: question,
        });
        self.follow = true;
        self.start_request(cx);
    }

    /// Asks the conversation's last question.
    fn start_request(&mut self, cx: &mut Context<Self>) {
        let Some(config) = AiConfig::get(cx) else {
            cx.emit(ChatEvent::OpenSettings);
            return;
        };
        // The request runs on its own thread (it blocks on the network); progress comes back over
        // a channel. Dropping the receiving end stops the request.
        let (progress_tx, mut progress_rx) = futures::channel::mpsc::unbounded();
        let (done_tx, mut done_rx) = futures::channel::oneshot::channel();
        let turns = self.turns.clone();
        let effort = self.effort;
        let spawned = std::thread::Builder::new()
            .name("inkveil-chat".into())
            .spawn(move || {
                let reply = ai::chat(&config.api_key, &turns, effort, &progress_tx);
                let _ = done_tx.send(reply);
            });
        if spawned.is_err() {
            self.status = Status::Failed("Could not start the request.".into());
            cx.notify();
            return;
        }
        self.status = Status::Working(Working::new(effort));
        self.request = Some(cx.spawn(async move |this, cx| {
            let mut answered = false;
            loop {
                cx.background_executor().timer(FRAME).await;
                // The reply may still be appearing on screen after the thread has finished.
                let outcome = if answered {
                    None
                } else {
                    match done_rx.try_recv() {
                        Ok(outcome) => outcome,
                        Err(_) => Some(Err(ai::AiError(
                            "The request stopped unexpectedly.".to_owned(),
                        ))),
                    }
                };
                answered |= outcome.is_some();
                // Read after the outcome: the thread reports everything before it finishes, so
                // the cost at the very end of a reply cannot be missed.
                let mut updates = Vec::new();
                while let Ok(progress) = progress_rx.try_recv() {
                    updates.push(progress);
                }
                let more = this.update(cx, |this, cx| this.on_frame(updates, outcome, cx));
                if !more.unwrap_or(false) {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// Takes in what the request reported and shows more of the reply. Returns false when the
    /// request is over.
    fn on_frame(
        &mut self,
        updates: Vec<Progress>,
        outcome: Option<Result<Zeroizing<String>, ai::AiError>>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Status::Working(working) = &mut self.status else {
            return false;
        };
        let tick = working.tick();
        let mut changed = !updates.is_empty();
        for progress in updates {
            working.apply(progress);
        }
        match outcome {
            Some(Ok(reply)) => working.finished = Some(reply),
            Some(Err(error)) => {
                self.status = Status::Failed(error.0);
                cx.notify();
                return false;
            }
            None => {}
        }
        changed |= working.reveal();
        if working.fully_shown()
            && let Some(reply) = working.finished.take()
        {
            if let Some(stats) = working.stats() {
                self.stats.insert(self.turns.len(), stats);
            }
            self.costs.add(working.effort, working.usage);
            self.turns.push(ChatTurn {
                from_user: false,
                text: reply,
            });
            self.status = Status::Idle;
            cx.notify();
            return false;
        }
        // The time on screen moves on even when nothing arrives.
        if changed || working.tick() != tick {
            cx.notify();
        }
        true
    }

    /// Stops the request. What has been written stays; a question with no answer yet goes back
    /// into the field, to change or send again.
    fn stop(&mut self, cx: &mut Context<Self>) {
        let status = std::mem::replace(&mut self.status, Status::Idle);
        self.request = None;
        self.selection = None;
        if let Status::Working(working) = status {
            self.costs.add(working.effort, working.usage);
            let written = ai::visible_text(&working.received);
            if !written.trim().is_empty() {
                if let Some(stats) = working.stats() {
                    self.stats.insert(self.turns.len(), stats);
                }
                self.turns.push(ChatTurn {
                    from_user: false,
                    text: Zeroizing::new(written),
                });
            } else if let Some(question) = self.turns.pop() {
                self.field
                    .update(cx, |field, cx| field.set_text(&question.text, cx));
            }
        }
        cx.notify();
    }

    /// Forgets the conversation.
    fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request = None;
        self.status = Status::Idle;
        self.turns.clear();
        self.stats.clear();
        self.costs.start_new_chat();
        self.selection = None;
        self.follow = true;
        self.copied = None;
        self.field.update(cx, TextField::clear);
        window.focus(&self.field.focus_handle(cx), cx);
        cx.notify();
    }

    fn copy(&mut self, text: &str, target: CopyTarget, cx: &mut Context<Self>) {
        // The check mark only shows for a copy that worked.
        if !crate::clipboard::write_text(text, cx) {
            return;
        }
        self.copied = Some(target);
        self.copied_reset = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            this.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The place in the conversation under the mouse, or the nearest one.
    fn position_at(&self, at: Point<Pixels>) -> Option<TextPos> {
        let blocks = self.blocks.borrow();
        for (index, block) in blocks.iter().enumerate() {
            let bounds = block.layout.bounds();
            if at.y < bounds.top() {
                return Some(TextPos {
                    block: index,
                    offset: 0,
                });
            }
            if at.y <= bounds.bottom() {
                let (Ok(offset) | Err(offset)) = block.layout.index_for_position(at);
                return Some(TextPos {
                    block: index,
                    offset: offset.min(block.text.len()),
                });
            }
        }
        let last = blocks.len().checked_sub(1)?;
        Some(TextPos {
            block: last,
            offset: blocks[last].text.len(),
        })
    }

    fn on_text_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.text_focus, cx);
        let selection = self.position_at(event.position).map(|pos| {
            let blocks = self.blocks.borrow();
            let text = &blocks[pos.block].text;
            let within = |range: std::ops::Range<usize>| TextSelection {
                anchor: TextPos {
                    block: pos.block,
                    offset: range.start,
                },
                head: TextPos {
                    block: pos.block,
                    offset: range.end,
                },
            };
            match (event.click_count, self.selection) {
                (0 | 1, Some(selection)) if event.modifiers.shift => TextSelection {
                    head: pos,
                    ..selection
                },
                (0 | 1, _) => TextSelection::at(pos),
                (2, _) => within(word_at(text, pos.offset)),
                _ => within(0..text.len()),
            }
        });
        self.selection = selection;
        self.selecting = event.click_count <= 1;
        cx.notify();
    }

    fn on_text_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        if !event.dragging() {
            self.selecting = false;
            return;
        }
        self.scroll_while_dragging(event.position);
        let head = self.position_at(event.position);
        if let (Some(selection), Some(head)) = (self.selection.as_mut(), head)
            && selection.head != head
        {
            selection.head = head;
            cx.notify();
        }
    }

    /// Dragging against the top or bottom of the conversation brings more of it into view.
    fn scroll_while_dragging(&self, at: Point<Pixels>) {
        let bounds = self.scroll.handle().bounds();
        let step = if at.y < bounds.top() + px(DRAG_SCROLL_EDGE) {
            DRAG_SCROLL_STEP
        } else if at.y > bounds.bottom() - px(DRAG_SCROLL_EDGE) {
            -DRAG_SCROLL_STEP
        } else {
            return;
        };
        let offset = self.scroll.handle().offset();
        let y = (offset.y + px(step)).clamp(-self.scroll.handle().max_offset().y, px(0.));
        self.scroll.handle().set_offset(point(offset.x, y));
    }

    fn on_text_up(&mut self, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        self.selecting = false;
        // Keep what was selected on screen while the reply goes on.
        if self.busy() && self.selection.is_some_and(|s| !s.is_empty()) {
            self.follow = false;
        }
        cx.notify();
    }

    fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let blocks = self.blocks.borrow();
        let text = selection.text(blocks.iter().map(|block| block.text.as_ref()));
        (!text.is_empty()).then_some(text)
    }

    fn copy_selection(&self, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            crate::clipboard::write_text(&text, cx);
        }
    }

    fn select_all_text(&mut self, cx: &mut Context<Self>) {
        let blocks = self.blocks.borrow();
        let selection = blocks.last().map(|last| TextSelection {
            anchor: TextPos {
                block: 0,
                offset: 0,
            },
            head: TextPos {
                block: blocks.len() - 1,
                offset: last.text.len(),
            },
        });
        drop(blocks);
        self.selection = selection;
        cx.notify();
    }

    /// Typing after selecting text goes on in the question field, without clicking it first.
    fn on_key_down(&self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let shortcut = keystroke.modifiers.control != keystroke.modifiers.alt;
        if !self.text_focus.is_focused(window) || shortcut {
            return;
        }
        let Some(typed) = keystroke
            .key_char
            .as_ref()
            .filter(|typed| !typed.chars().any(char::is_control))
        else {
            return;
        };
        let question = Zeroizing::new(format!("{}{typed}", self.field.read(cx).text()));
        self.field
            .update(cx, |field, cx| field.set_text(&question, cx));
        window.focus(&self.field.focus_handle(cx), cx);
    }

    /// Reading older messages stops the view following the reply; coming back resumes it.
    fn on_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let moved = event.delta.pixel_delta(px(20.)).y;
        if moved > px(0.) {
            self.follow = false;
        } else if moved < px(0.) {
            let left =
                self.scroll.handle().max_offset().y + self.scroll.handle().offset().y + moved;
            self.follow = left <= px(NEAR_END);
        }
        cx.notify();
    }
}

// ----- In the workspace --------------------------------------------------------------------------

impl Workspace {
    /// The chat, created the first time it is opened. One conversation is shared by all notes.
    fn chat_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<ChatPanel> {
        if let Some(chat) = &self.chat {
            return chat.clone();
        }
        let chat = cx.new(|cx| ChatPanel::new(window, cx));
        cx.subscribe_in(&chat, window, |this, _, event, window, cx| match event {
            ChatEvent::Close => this.close_chat(window, cx),
            ChatEvent::OpenSettings => this.open_ai_settings(
                Some("Add your OpenRouter API key to use the chat."),
                window,
                cx,
            ),
        })
        .detach();
        self.chat = Some(chat.clone());
        chat
    }

    /// Opens the chat in the editor of `tab`, or closes it if it is there already. Without a
    /// note open it fills the empty area instead.
    pub(super) fn toggle_chat(
        &mut self,
        tab: Option<Entity<super::note_editor::NoteEditor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open_here = match &tab {
            Some(tab) => tab.read(cx).chat_open(),
            None => self.chat_alone,
        };
        if open_here {
            self.close_chat(window, cx);
            return;
        }
        self.hide_chat(cx);
        let chat = self.chat_panel(window, cx);
        match tab {
            Some(tab) => tab.update(cx, |note, cx| note.show_chat(chat.clone(), cx)),
            None => self.chat_alone = true,
        }
        window.focus(&chat.focus_handle(cx), cx);
        cx.notify();
    }

    /// Takes the chat off the screen. The conversation stays until the vault locks.
    fn hide_chat(&mut self, cx: &mut Context<Self>) {
        self.chat_alone = false;
        for tab in &self.tabs {
            tab.update(cx, super::note_editor::NoteEditor::hide_chat);
        }
    }

    fn close_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_chat(cx);
        match self.active {
            Some(active) => self.tabs[active].update(cx, |note, cx| note.focus(window, cx)),
            None => window.focus(&self.focus_handle, cx),
        }
        cx.notify();
    }

    /// Wipes the conversation and stops any request.
    pub(super) fn forget_chat(&mut self, cx: &mut Context<Self>) {
        self.hide_chat(cx);
        self.chat = None;
    }
}

#[cfg(test)]
impl ChatPanel {
    /// Puts a message in the panel, as if it had been sent or had arrived.
    pub fn add_turn(&mut self, from_user: bool, text: &str) {
        self.turns.push(ChatTurn {
            from_user,
            text: Zeroizing::new(text.to_owned()),
        });
    }

    /// Records how long the last message took, as if the reply had just arrived.
    pub fn add_stats(&mut self, thought: Duration, total: Duration) {
        let stats = ReplyStats {
            thought,
            total,
            effort: self.effort,
            usage: None,
        };
        self.stats.insert(self.turns.len() - 1, stats);
    }

    /// Records a request in the cost panel, as if it had finished.
    pub fn add_cost(&mut self, cost: f64) {
        self.costs.add(self.effort, Some(ai::Usage::costing(cost)));
    }

    pub fn selection_text(&self) -> Option<String> {
        self.selected_text()
    }

    pub fn field_selection(&self, cx: &App) -> String {
        self.field.read(cx).selected_text().to_owned()
    }

    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }

    pub fn asks_about_secrets(&self) -> bool {
        matches!(self.status, Status::ConfirmSecrets(_))
    }

    pub fn set_question(&mut self, text: &str, cx: &mut Context<Self>) {
        self.field.update(cx, |field, cx| field.set_text(text, cx));
    }

    pub fn question(&self, cx: &App) -> String {
        self.field.read(cx).text().to_owned()
    }

    pub fn effort(&self) -> Effort {
        self.effort
    }

    /// Makes the panel look as if a request were running, without any network.
    pub fn pretend_working(&mut self, received: &str) {
        let mut working = Working::new(self.effort);
        working.received = Zeroizing::new(received.to_owned());
        working.shown = received.len();
        working.thought = (!received.is_empty()).then_some(Duration::from_secs(1));
        self.status = Status::Working(working);
    }

    pub fn press_stop(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
    }

    /// How far down the conversation is scrolled, and how far it can go.
    pub fn scrolled(&self) -> (Pixels, Pixels) {
        let scroll = self.scroll.handle();
        (-scroll.offset().y, scroll.max_offset().y)
    }

    pub fn follows_the_end(&self) -> bool {
        self.follow
    }
}

// ----- Rendering ---------------------------------------------------------------------------------

/// A centered column of the conversation's width.
fn column() -> gpui::Div {
    div().w_full().max_w(px(COLUMN_WIDTH)).min_w_0()
}

impl ChatPanel {
    /// `line` as text that can be selected, with its bold and code parts styled. Blocks must be
    /// rendered from the top down: their order is how a selection finds them.
    fn text_block(&self, line: &Inline, colors: &Theme) -> gpui::Div {
        let index = self.blocks.borrow().len();
        let overrides: Vec<_> = line
            .marks
            .iter()
            .filter(|(_, mark)| *mark == Mark::Code)
            .map(|(range, _)| (range.clone(), SharedString::from(MONO_FONT)))
            .collect();
        let background = colors.code_background;
        let marks = line.marks.iter().map(|(range, mark)| {
            (
                range.clone(),
                match mark {
                    Mark::Bold => HighlightStyle {
                        font_weight: Some(gpui::FontWeight::SEMIBOLD),
                        ..Default::default()
                    },
                    Mark::Code => HighlightStyle {
                        background_color: Some(background),
                        ..Default::default()
                    },
                },
            )
        });
        let selected = self
            .selection
            .and_then(|selection| selection.range_in(index, line.text.len()))
            .map(|range| {
                (
                    range,
                    HighlightStyle {
                        background_color: Some(colors.selection),
                        ..Default::default()
                    },
                )
            });
        let highlights: Vec<_> = combine_highlights(marks, selected).collect();
        let shared = SharedString::from(line.text.clone());
        let text = StyledText::new(shared.clone())
            .with_highlights(highlights)
            .with_font_family_overrides(overrides);
        self.blocks.borrow_mut().push(TextBlock {
            text: shared,
            layout: text.layout().clone(),
        });
        div()
            .debug_selector(|| format!("chat-text-{index}"))
            .cursor(CursorStyle::IBeam)
            .child(text)
    }

    /// A small icon button below a reply or on a code block.
    fn copy_button(
        &self,
        id: SharedString,
        target: CopyTarget,
        text: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let colors = theme(cx);
        let copied = self.copied == Some(target);
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(24.))
            .rounded_md()
            .cursor_pointer()
            .hover(|d| d.bg(colors.hover))
            .tooltip(|_, cx| Tooltip::view("Copy", cx))
            .child(
                icon(
                    if copied { Icon::Check } else { Icon::Copy },
                    if copied { colors.accent } else { colors.muted },
                )
                .size(px(14.)),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.copy(&text, target, cx)))
    }

    /// A reply, or the part of it on screen so far (`turn` is `None` while it is being written).
    fn render_reply(
        &self,
        text: &str,
        turn: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let colors = theme(cx);
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .min_w_0()
            .text_size(px(14.))
            .line_height(px(22.));
        for (index, block) in chat_text::blocks(text).into_iter().enumerate() {
            body = body.child(match block {
                Block::Paragraph(line) => self.text_block(&line, &colors).into_any_element(),
                Block::Heading(line) => self
                    .text_block(&line, &colors)
                    .pt_1()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .into_any_element(),
                Block::Item(marker, line) => div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(14.))
                            .text_color(colors.muted)
                            .child(marker),
                    )
                    .child(self.text_block(&line, &colors).flex_1().min_w_0())
                    .into_any_element(),
                Block::Code(code) => self.render_code(&code, turn, index, cx),
            });
        }
        let footer = turn.map(|turn| {
            let stats = self.stats.get(&turn).copied();
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(self.copy_button(
                    format!("copy-reply-{turn}").into(),
                    CopyTarget::Reply(turn),
                    text.to_owned(),
                    cx,
                ))
                .children(stats.map(|stats| {
                    let cost = stats
                        .usage
                        .and_then(|usage| usage.cost)
                        .map_or_else(String::new, |cost| format!(" · {}", cost_label(cost)));
                    div()
                        .debug_selector(|| format!("chat-stats-{turn}"))
                        .text_size(px(12.))
                        .text_color(colors.muted)
                        .child(format!(
                            "Thought for {} · {} in all · {}{cost}",
                            time_label(stats.thought),
                            time_label(stats.total),
                            stats.effort.label()
                        ))
                }))
        });
        div().flex().flex_col().gap_1().child(body).children(footer)
    }

    fn render_code(
        &self,
        code: &str,
        turn: Option<usize>,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme(cx);
        let text = self.text_block(
            &Inline {
                text: code.to_owned(),
                marks: Vec::new(),
            },
            &colors,
        );
        let copy = turn.map(|turn| {
            div()
                .absolute()
                .top(px(3.))
                .right(px(3.))
                .child(self.copy_button(
                    format!("copy-code-{turn}-{index}").into(),
                    CopyTarget::Code(turn, index),
                    code.to_owned(),
                    cx,
                ))
        });
        div()
            .relative()
            .w_full()
            .p_2()
            .pr(px(30.))
            .rounded_md()
            .bg(colors.code_background)
            .font_family(MONO_FONT)
            .text_size(px(12.))
            .line_height(px(18.))
            .child(text)
            .children(copy)
            .into_any_element()
    }

    fn render_question(&self, text: &str, colors: &Theme) -> impl IntoElement + use<> {
        let line = Inline {
            text: text.to_owned(),
            marks: Vec::new(),
        };
        div().flex().justify_end().child(
            div()
                .max_w(relative(0.8))
                .px_3()
                .py(px(7.))
                .rounded_lg()
                .bg(colors.selected)
                .text_size(px(14.))
                .line_height(px(21.))
                .child(self.text_block(&line, colors)),
        )
    }

    /// What the model is doing, with the time so far.
    fn render_progress(working: &Working, colors: &Theme) -> impl IntoElement + use<> {
        let elapsed = time_label(working.attempt_started.elapsed());
        let retry = if working.retry > 0 {
            format!("Retry {} of {}. ", working.retry, ai::MAX_RETRIES)
        } else {
            String::new()
        };
        let (text, paused) = match (&working.phase, working.thought) {
            (Phase::Pause { reason, until }, _) => {
                let left = until.saturating_duration_since(Instant::now()).as_secs() + 1;
                (format!("{reason}. Trying again in {left} s."), true)
            }
            (_, Some(thought)) => (
                format!("{retry}Thought for {}. Writing…", time_label(thought)),
                false,
            ),
            (Phase::Waiting, None) => (format!("{retry}Connecting… {elapsed}"), false),
            (_, None) => (format!("{retry}Thinking… {elapsed}"), false),
        };
        div()
            .debug_selector(|| "chat-progress".to_owned())
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(12.))
            .text_color(if paused { colors.error } else { colors.muted })
            .child(
                div()
                    .size(px(7.))
                    .flex_none()
                    .rounded_full()
                    .bg(if paused { colors.muted } else { colors.accent })
                    .with_animation(
                        "chat-pulse",
                        Animation::new(Duration::from_millis(1100))
                            .repeat()
                            .with_easing(pulsating_between(0.25, 1.)),
                        gpui::Styled::opacity,
                    ),
            )
            .child(text)
    }

    fn render_empty(&self, has_key: bool, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme(cx);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px_6()
            .child(
                div()
                    .size(px(44.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(colors.selected)
                    .child(icon(Icon::Grok, colors.text).size(px(24.))),
            )
            .child(
                div()
                    .pt_1()
                    .text_size(px(16.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Ask Grok anything"),
            )
            .child(
                div()
                    .max_w(px(420.))
                    .text_center()
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .text_color(colors.muted)
                    .child(if has_key {
                        "Only what you type here is sent, with zero data retention. This chat is \
                         never saved: it disappears when you lock or close Inkveil."
                    } else {
                        "Add your OpenRouter API key to start. It stays encrypted in your vault."
                    }),
            )
            .when(!has_key, |d| {
                d.child(
                    div().pt_2().child(
                        button("chat-add-key", "Add API key", true, cx)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatEvent::OpenSettings))),
                    ),
                )
            })
            .into_any_element()
    }

    /// The card under the conversation that asks for a decision or reports a problem.
    fn render_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = theme(cx);
        match &self.status {
            Status::ConfirmSecrets(findings) => Some(
                div()
                    .debug_selector(|| "chat-secrets".to_owned())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(colors.error)
                    .text_size(px(12.))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(colors.error)
                            .child("This may contain secrets"),
                    )
                    .children(
                        findings
                            .iter()
                            .map(|f| div().child(format!("• {} ({})", f.kind, f.hint))),
                    )
                    .child(div().text_color(colors.muted).child(
                        "Sending would share them with OpenRouter and the model provider. Nothing \
                         has been sent.",
                    ))
                    .child(
                        div().flex().justify_end().child(
                            button("chat-send-anyway", "Send anyway", false, cx)
                                .h(px(28.))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.send_unchecked(cx);
                                })),
                        ),
                    )
                    .into_any_element(),
            ),
            Status::Failed(message) => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .child(div().text_color(colors.error).child(message.clone()))
                    .child(
                        div().child(
                            button("chat-try-again", "Try again", false, cx)
                                .h(px(28.))
                                .w(px(96.))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.follow = true;
                                    this.start_request(cx);
                                })),
                        ),
                    )
                    .into_any_element(),
            ),
            Status::Notice(message) => Some(
                div()
                    .text_size(px(12.))
                    .text_color(colors.error)
                    .child(*message)
                    .into_any_element(),
            ),
            Status::Idle | Status::Working(_) => None,
        }
    }

    /// The model, with its logo.
    fn render_model(cx: &App) -> impl IntoElement + use<> {
        let colors = theme(cx);
        div()
            .id("chat-model")
            .debug_selector(|| "chat-model".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .h(px(26.))
            .px_2()
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .text_size(px(12.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .tooltip(|_, cx| Tooltip::view(ai::CHAT_MODEL, cx))
            .child(icon(Icon::Grok, colors.text).size(px(14.)))
            .child("Grok 4.7")
    }

    /// The thinking levels.
    fn render_thinking(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let colors = theme(cx);
        div()
            .id("chat-thinking")
            .flex()
            .flex_none()
            .h(px(26.))
            .p(px(2.))
            .gap(px(2.))
            .rounded_md()
            .bg(colors.hover)
            .tooltip(|_, cx| Tooltip::view("Thinking: higher is slower and costs more", cx))
            .children(ai::CHAT_EFFORTS.iter().map(|&effort| {
                let selected = self.effort == effort;
                let id = format!("chat-effort-{}", effort.label());
                div()
                    .debug_selector(|| id.clone())
                    .id(SharedString::from(id.clone()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_2()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_size(px(11.))
                    .text_color(if selected { colors.text } else { colors.muted })
                    .when(selected, |d| {
                        d.bg(colors.background)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                    })
                    .when(!selected, |d| d.hover(|d| d.text_color(colors.text)))
                    .child(effort.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.effort = effort;
                        cx.notify();
                    }))
            }))
    }

    fn render_send(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let colors = theme(cx);
        let busy = self.busy();
        let can_send = !self.field.read(cx).text().trim().is_empty();
        div()
            .id("chat-send")
            .debug_selector(|| "chat-send".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(30.))
            .rounded_md()
            .when(busy || can_send, |d| {
                d.bg(colors.accent)
                    .cursor_pointer()
                    .hover(|d| d.opacity(0.9))
            })
            .when(!busy && !can_send, |d| d.bg(colors.hover))
            .tooltip(move |_, cx| Tooltip::view(if busy { "Stop" } else { "Send (Enter)" }, cx))
            .child(
                icon(
                    if busy { Icon::Stop } else { Icon::ArrowUp },
                    if busy || can_send {
                        gpui::white()
                    } else {
                        colors.muted
                    },
                )
                .size(px(15.)),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if this.busy() {
                    this.stop(cx);
                } else {
                    this.send(cx);
                }
            }))
    }

    fn render_composer(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let colors = theme(cx);
        let focused = self.field.focus_handle(cx).is_focused(window);
        div()
            .flex()
            .flex_none()
            .justify_center()
            .px_4()
            .pb_4()
            .pt_2()
            .child(
                column()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(if focused {
                        colors.accent
                    } else {
                        colors.border
                    })
                    .bg(colors.input)
                    .shadow_sm()
                    .child(
                        div()
                            .debug_selector(|| "chat-input".to_owned())
                            .text_size(px(14.))
                            .line_height(px(22.))
                            .child(self.field.clone()),
                    )
                    .child(
                        // Wraps in the narrow half of a split view.
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(Self::render_model(cx))
                            .child(self.render_thinking(cx))
                            .child(div().flex_1())
                            .child(self.render_send(cx)),
                    ),
            )
    }

    /// One request in the cost panel.
    fn render_request_cost(
        number: usize,
        request: RequestCost,
        colors: &Theme,
    ) -> impl IntoElement + use<> {
        let details = request.usage.map_or_else(
            || "Stopped: cost not reported".to_owned(),
            |usage| {
                let thinking = match usage.reasoning_tokens() {
                    0 => String::new(),
                    tokens => format!(" · {tokens} thinking"),
                };
                format!(
                    "{} in · {} out{thinking}",
                    usage.prompt_tokens, usage.completion_tokens
                )
            },
        );
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .px_3()
            .py_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(format!("Request {number} · {}", request.effort.label()))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(request.cost().map_or("?".to_owned(), cost_label)),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(details),
            )
    }

    /// What the requests cost, beside the conversation.
    fn render_costs(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let colors = theme(cx);
        let count = self.costs.requests.len();
        let requests = self.costs.requests.iter().enumerate().rev();
        div()
            .debug_selector(|| "chat-costs".to_owned())
            .w(px(COSTS_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .text_size(px(12.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(colors.muted)
                            .child("COST OF THIS CHAT"),
                    )
                    .child(
                        div()
                            .debug_selector(|| "chat-cost-total".to_owned())
                            .text_size(px(22.))
                            .line_height(px(30.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(cost_label(self.costs.chat_total())),
                    )
                    .child(div().text_color(colors.muted).child(format!(
                        "{count} {} · {} since unlocking",
                        if count == 1 { "request" } else { "requests" },
                        cost_label(self.costs.session_total())
                    ))),
            )
            .child(hairline(cx))
            .child(
                div()
                    .id("chat-cost-list")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .when(count == 0, |d| {
                        d.child(
                            div()
                                .p_3()
                                .text_color(colors.muted)
                                .child("Each request appears here with what it cost."),
                        )
                    })
                    .children(requests.map(|(index, request)| {
                        Self::render_request_cost(index + 1, *request, &colors)
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .p_3()
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .text_color(colors.muted)
                    .child(if self.costs.has_unknown() {
                        "A stopped request is still billed but not reported, so the real total \
                         is a little higher."
                    } else {
                        "As reported by OpenRouter. Longer chats cost more, since each question \
                         sends the conversation again."
                    }),
            )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let colors = theme(cx);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(36.))
            .pl_3()
            .pr_2()
            .child(
                div()
                    .id("chat-zdr")
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .tooltip(|_, cx| {
                        Tooltip::view("Requests only go to endpoints with zero data retention", cx)
                    })
                    .child(icon(Icon::ShieldCheck, colors.accent).size(px(13.)))
                    .child("Zero data retention · Not saved"),
            )
            .child(div().flex_1())
            .when(!self.turns.is_empty() || self.busy(), |d| {
                d.child(
                    icon_button("chat-new", Icon::Plus, false, "New chat", cx)
                        .on_click(cx.listener(|this, _, window, cx| this.new_chat(window, cx))),
                )
            })
            .child(
                icon_button(
                    "chat-show-costs",
                    Icon::DollarSign,
                    self.show_costs,
                    "Show or hide what requests cost",
                    cx,
                )
                .debug_selector(|| "chat-show-costs".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_costs = !this.show_costs;
                    cx.notify();
                })),
            )
            .child(
                icon_button(
                    "chat-close",
                    Icon::X,
                    false,
                    "Back to the note (Escape)",
                    cx,
                )
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatEvent::Close))),
            )
    }
}

impl Render for ChatPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        let has_key = AiConfig::get(cx).is_some();
        let empty = self.turns.is_empty() && !self.busy();
        if self.scroll.dragging() {
            // Dragging the scrollbar away from the end stops the view following the reply;
            // dragging it back to the end resumes that.
            let scroll = self.scroll.handle();
            self.follow = scroll.max_offset().y + scroll.offset().y <= px(NEAR_END);
        }
        if self.follow && !self.selecting {
            self.scroll.handle().scroll_to_bottom();
        }
        self.blocks.get_mut().clear();
        let mut conversation: Vec<AnyElement> = Vec::new();
        for (index, turn) in self.turns.iter().enumerate() {
            conversation.push(if turn.from_user {
                self.render_question(&turn.text, &colors).into_any_element()
            } else {
                self.render_reply(&turn.text, Some(index), cx)
                    .into_any_element()
            });
        }
        if let Status::Working(working) = &self.status {
            let shown = ai::visible_text(&working.received[..working.shown]);
            if !shown.is_empty() {
                conversation.push(self.render_reply(&shown, None, cx).into_any_element());
            }
            conversation.push(Self::render_progress(working, &colors).into_any_element());
        }
        conversation.extend(self.render_status(cx));

        div()
            .id("chat-panel")
            .debug_selector(|| "chat-panel".to_owned())
            .key_context(CONTEXT)
            .track_focus(&self.text_focus)
            .on_action(cx.listener(|_, _: &CloseChat, _, cx| cx.emit(ChatEvent::Close)))
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.copy_selection(cx)))
            .on_action(cx.listener(|this, _: &SelectAllText, _, cx| this.select_all_text(cx)))
            .on_key_down(cx.listener(|this, event, window, cx| {
                this.on_key_down(event, window, cx);
            }))
            .on_mouse_move(cx.listener(|this, event, _, cx| this.on_text_move(event, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.on_text_up(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.on_text_up(cx)),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .text_color(colors.text)
            .child(self.render_header(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(if empty && conversation.is_empty() {
                                self.render_empty(has_key, cx)
                            } else {
                                div()
                                    .relative()
                                    .flex_1()
                                    .min_h_0()
                                    .child(
                                        div()
                                            .id("chat-messages")
                                            .debug_selector(|| "chat-messages".to_owned())
                                            .size_full()
                                            .flex()
                                            .flex_col()
                                            .items_center()
                                            .px_4()
                                            .py_2()
                                            .overflow_y_scroll()
                                            .track_scroll(self.scroll.handle())
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(Self::on_text_down),
                                            )
                                            .on_scroll_wheel(cx.listener(|this, event, _, cx| {
                                                this.on_scroll(event, cx);
                                            }))
                                            .child(
                                                column()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_4()
                                                    .children(conversation),
                                            ),
                                    )
                                    .child(self.scroll.render(cx))
                                    .when(!self.follow, |d| {
                                        d.child(
                                            div()
                                                .absolute()
                                                .bottom(px(8.))
                                                .left_0()
                                                .right_0()
                                                .flex()
                                                .justify_center()
                                                .child(
                                                    div()
                                                        .rounded_full()
                                                        .bg(colors.background)
                                                        .border_1()
                                                        .border_color(colors.border)
                                                        .shadow_md()
                                                        .child(
                                                            icon_button(
                                                                "chat-jump",
                                                                Icon::ArrowDown,
                                                                false,
                                                                "Go to the latest message",
                                                                cx,
                                                            )
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| {
                                                                    this.follow = true;
                                                                    cx.notify();
                                                                },
                                                            )),
                                                        ),
                                                ),
                                        )
                                    })
                                    .into_any_element()
                            })
                            .child(self.render_composer(window, cx)),
                    )
                    .when(self.show_costs, |d| d.child(self.render_costs(cx))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn working(text: &str) -> Working {
        let mut working = Working::new(Effort::Low);
        working.received = Zeroizing::new(text.to_owned());
        working
    }

    #[test]
    fn replies_appear_little_by_little_and_end_complete() {
        let mut w = working("Përshëndetje nga një përgjigje e gjatë me shkronja të veçanta ë ç");
        let mut frames = 0;
        while w.reveal() {
            frames += 1;
            assert!(w.received.is_char_boundary(w.shown));
            assert!(frames < 100, "it must finish");
        }
        assert!(frames > 3, "not all at once");
        assert!(w.fully_shown());
    }

    #[test]
    fn a_big_backlog_catches_up_quickly() {
        assert_eq!(reveal_step(1), 3);
        assert_eq!(reveal_step(600), 100);
    }

    #[test]
    fn a_new_attempt_starts_the_reply_and_the_thinking_time_over() {
        let mut w = Working::new(Effort::High);
        w.apply(Progress::Piece(Zeroizing::new("half a repl".to_owned())));
        assert!(w.thought.is_some());
        w.reveal();
        w.apply(Progress::Waiting { attempt: 2 });
        assert_eq!(w.retry, 1);
        assert_eq!(w.received.as_str(), "");
        assert_eq!(w.shown, 0);
        assert_eq!(w.thought, None);
        w.apply(Progress::Piece(Zeroizing::new("whole".to_owned())));
        assert_eq!(w.received.as_str(), "whole");
    }

    #[test]
    fn thinking_time_stops_at_the_first_word() {
        let mut w = Working::new(Effort::Medium);
        assert_eq!(w.stats(), None, "nothing to report before the first word");
        w.apply(Progress::Piece(Zeroizing::new("a".to_owned())));
        let first = w.thought;
        std::thread::sleep(Duration::from_millis(5));
        w.apply(Progress::Piece(Zeroizing::new("b".to_owned())));
        assert_eq!(w.thought, first);
        let stats = w.stats().expect("started");
        assert!(stats.total > stats.thought);
        assert_eq!(stats.effort, Effort::Medium);
    }

    #[test]
    fn amounts_read_naturally() {
        assert_eq!(cost_label(0.), "$0");
        assert_eq!(cost_label(0.000_01), "<$0.0001");
        assert_eq!(cost_label(0.00123), "$0.0012");
        assert_eq!(cost_label(1.256), "$1.26");
    }

    #[test]
    fn totals_add_up_and_a_new_chat_starts_at_zero() {
        let mut log = CostLog::default();
        log.add(Effort::Low, Some(ai::Usage::costing(0.25)));
        log.add(Effort::High, Some(ai::Usage::costing(0.5)));
        assert!((log.chat_total() - 0.75).abs() < f64::EPSILON);
        assert!(!log.has_unknown());

        log.start_new_chat();
        assert!(log.chat_total().abs() < f64::EPSILON);
        log.add(Effort::Low, Some(ai::Usage::costing(0.25)));
        assert!((log.chat_total() - 0.25).abs() < f64::EPSILON);
        assert!((log.session_total() - 1.).abs() < f64::EPSILON);
    }

    #[test]
    fn a_stopped_request_counts_as_unknown_not_free() {
        let mut log = CostLog::default();
        log.add(Effort::Low, None);
        assert!(log.has_unknown());
        assert_eq!(log.requests[0].cost(), None);
    }

    #[test]
    fn a_retry_forgets_the_usage_of_the_failed_attempt() {
        let mut w = Working::new(Effort::Low);
        w.apply(Progress::Usage(ai::Usage::default()));
        assert!(w.usage.is_some());
        w.apply(Progress::Waiting { attempt: 2 });
        assert_eq!(w.usage, None);
    }

    #[test]
    fn times_read_naturally() {
        assert_eq!(time_label(Duration::from_millis(3400)), "3.4 s");
        assert_eq!(time_label(Duration::from_millis(12_600)), "12 s");
        assert_eq!(time_label(Duration::from_secs(125)), "2 min 5 s");
    }
}
