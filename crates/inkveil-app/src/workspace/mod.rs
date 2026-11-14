//! The unlocked app: sidebar with notes, tabs of open notes, the editor and a status bar.

mod ai_panel;
mod ai_settings;
mod backups;
mod chat;
mod chat_selection;
mod chat_text;
mod find;
mod history;
mod note_editor;
mod pictures;
mod sections;
mod session;
mod sidebar;
mod tab_layout;
mod tabs;
#[cfg(test)]
mod ui_tests;

use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime},
};

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, MouseDownEvent, Render,
    SharedString, Task, WeakEntity, Window, actions, anchored, deferred, div, prelude::*, px,
};
use inkveil_doc::{Document, Pos};
use inkveil_store::{NoteId, NoteMeta, NoteSummary, StoreError, Vault, now_ms};

use self::{
    note_editor::{NoteEditor, NoteEditorEvent},
    session::{OpenTab, Session},
    sidebar::SidebarState,
    tab_layout::TabLayout,
    tabs::TabBarState,
};
use crate::{
    app::InkveilApp,
    assets::Icon,
    editor::{EditorSettings, RichClipboard},
    search::BodyHit,
    settings::{AppSettings, AutoLock, should_lock},
    spell,
    storage::{Storage, StorageEvent, state_keys},
    theme::theme,
    ui::{icon, text_field::TextField},
};

actions!(
    workspace,
    [
        NewNote,
        CloseTab,
        ReopenClosedTab,
        ToggleSplit,
        NextTab,
        PrevTab,
        FocusSearch,
        LockVault,
        TrashNote,
        ToggleWrap,
        ToggleLineNumbers,
        ToggleChat,
        SaveNow
    ]
);

const CONTEXT: &str = "Workspace";
/// Trashed notes are deleted for good after this many days.
const TRASH_DAYS: i64 = 30;
const TICK: Duration = Duration::from_millis(250);

/// What is open on top of the notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlay {
    None,
    /// A menu, the picture viewer or the recovery key: nothing to type into.
    Menu,
    /// The AI settings or the link popup: they have text fields.
    Dialog,
}

/// Keys that, with Ctrl, edit text in a field (the clipboard, undo, moving by word or to the
/// ends). Every other Ctrl or Alt shortcut is kept away from what is under a dialog.
const EDITING_KEYS: [&str; 13] = [
    "a",
    "c",
    "v",
    "x",
    "z",
    "y",
    "left",
    "right",
    "backspace",
    "delete",
    "home",
    "end",
    "insert",
];

/// How often a caret that moved, with nothing else about the open tabs changing, is stored.
const CARET_SAVE_EVERY: Duration = Duration::from_secs(10);

/// How long quitting waits for the last saves. Housekeeping is skipped by then, so this is only
/// reached when the disk itself is very slow.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(30);

pub fn key_bindings() -> Vec<gpui::KeyBinding> {
    use gpui::KeyBinding;
    let ctx = Some(CONTEXT);
    let mut bindings = find::key_bindings();
    bindings.extend(chat::key_bindings());
    bindings.extend([
        KeyBinding::new("ctrl-n", NewNote, ctx),
        KeyBinding::new("ctrl-w", CloseTab, ctx),
        KeyBinding::new("ctrl-f4", CloseTab, ctx),
        KeyBinding::new("ctrl-shift-t", ReopenClosedTab, ctx),
        KeyBinding::new("ctrl-shift-s", ToggleSplit, ctx),
        KeyBinding::new("ctrl-tab", NextTab, ctx),
        KeyBinding::new("ctrl-pagedown", NextTab, ctx),
        KeyBinding::new("ctrl-shift-tab", PrevTab, ctx),
        KeyBinding::new("ctrl-pageup", PrevTab, ctx),
        KeyBinding::new("ctrl-p", FocusSearch, ctx),
        KeyBinding::new("ctrl-l", LockVault, ctx),
        KeyBinding::new("ctrl-j", ToggleChat, ctx),
        // Notes save themselves; this is for the reflex, and saves at once.
        KeyBinding::new("ctrl-s", SaveNow, ctx),
        // Same as VS Code.
        KeyBinding::new("alt-z", ToggleWrap, ctx),
        KeyBinding::new("alt-n", ToggleLineNumbers, ctx),
        KeyBinding::new("ctrl-shift-f", FocusSearch, ctx),
    ]);
    bindings
}

#[allow(clippy::struct_excessive_bools)]
pub struct Workspace {
    app: WeakEntity<InkveilApp>,
    storage: Storage,
    notes: Vec<NoteSummary>,
    loaded: bool,
    tabs: Vec<Entity<NoteEditor>>,
    active: Option<usize>,
    /// Pinned tabs and split views.
    layout: TabLayout<NoteId>,
    /// Pins and splits from the stored session whose tabs have not opened yet.
    restoring: TabLayout<NoteId>,
    /// A note opening in a split view, and the open note it goes beside.
    pending_split: Option<(NoteId, NoteId)>,
    tab_bar: TabBarState,
    search: Entity<TextField>,
    /// Notes whose text matches the sidebar search.
    body_hits: HashMap<NoteId, BodyHit>,
    search_task: Option<Task<()>>,
    /// A note being opened from a search result, and the text to find in it.
    pending_find: Option<(NoteId, String)>,
    show_trash: bool,
    sidebar: SidebarState,
    last_time_refresh: Instant,
    confirm_delete: Option<(NoteId, Instant)>,
    save_error: Option<SharedString>,
    /// A problem the user must not miss (saving stopped); cannot be dismissed.
    fatal_error: Option<SharedString>,
    /// Trashed notes older than [`TRASH_DAYS`], waiting for the user to confirm deletion.
    expired_trash: Vec<NoteId>,
    /// A freshly created recovery key being shown to the user.
    new_recovery_key: Option<zeroize::Zeroizing<String>>,
    ai_dialog: Option<ai_settings::AiDialog>,
    /// The chat with its conversation, from the first time it opens until the vault locks.
    chat: Option<Entity<chat::ChatPanel>>,
    /// The chat is showing with no note open.
    chat_alone: bool,
    backups: backups::BackupState,
    session_dirty: bool,
    saved_session: Option<Session>,
    last_session_write: Instant,
    settings: AppSettings,
    show_settings: bool,
    /// The settings menu was opened from the sidebar's gear (else from the status bar).
    settings_from_sidebar: bool,
    /// The mouse press being handled is on a button that opens the settings menu. The menu
    /// then leaves closing to that button: closing on the press and toggling on the click
    /// would open it again.
    press_on_settings: bool,
    last_activity: Instant,
    last_activity_wall: SystemTime,
    last_tick_wall: SystemTime,
    last_session_check: Instant,
    session_locked: Option<bool>,
    spell_generation: u64,
    focus_handle: FocusHandle,
    _tasks: Vec<Task<()>>,
    _keystrokes: [gpui::Subscription; 2],
}

impl Workspace {
    pub fn new(
        app: WeakEntity<InkveilApp>,
        vault: Vault,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (events_tx, mut events_rx) = futures::channel::mpsc::unbounded::<StorageEvent>();
        let storage = Storage::start(vault, events_tx);
        crate::storage::set_active(Some(storage.clone()));

        let search = cx.new(|cx| TextField::new("Search notes", window, cx));
        cx.subscribe_in(&search, window, |this, _, event, window, cx| {
            match event {
                crate::ui::text_field::TextFieldEvent::Changed => {
                    this.sidebar.show_all = false;
                    this.schedule_body_search(cx);
                }
                crate::ui::text_field::TextFieldEvent::Submitted => {
                    this.open_first_result(window, cx);
                }
                crate::ui::text_field::TextFieldEvent::Blurred => {}
            }
            cx.notify();
        })
        .detach();

        let load = cx.spawn_in(window, {
            let storage = storage.clone();
            async move |this, cx| {
                let notes = storage.list_notes().await;
                let session = storage.get_state(state_keys::SESSION).await;
                let settings = storage.get_state(state_keys::SETTINGS).await;
                let dictionary = storage.get_state(state_keys::DICTIONARY).await;
                let sections = storage.get_state(state_keys::SECTIONS).await;
                let ai = storage.get_state(state_keys::AI).await;
                let backup_folder = storage.get_state(state_keys::BACKUP_FOLDER).await;
                this.update_in(cx, |this, window, cx| match notes {
                    Ok(notes) => {
                        if let Ok(Some(bytes)) = settings {
                            this.settings = AppSettings::decode(&bytes);
                            cx.set_global(this.settings.editor);
                        }
                        if let Ok(Some(bytes)) = sections {
                            this.sidebar.sections = sections::decode(&bytes);
                        }
                        this.load_ai_config(ai.ok().flatten().as_deref().map(|b| &b[..]), cx);
                        if let Ok(Some(bytes)) = dictionary {
                            let words = String::from_utf8_lossy(&bytes)
                                .lines()
                                .map(str::to_owned)
                                .collect();
                            spell::set_personal(words);
                        }
                        if !notes.unreadable.is_empty() {
                            this.save_error = Some(
                                format!(
                                    "{} note(s) could not be read (damaged or altered). They are left untouched; \
                                     your other notes are fine. Backups are in {}.",
                                    notes.unreadable.len(),
                                    this.storage_backup_dir()
                                )
                                .into(),
                            );
                        }
                        this.notes = notes.notes;
                        this.load_backup_folder(
                            backup_folder.ok().flatten().as_deref().map(|b| &b[..]),
                        );
                        this.loaded = true;
                        this.purge_old_trash();
                        let session = session
                            .ok()
                            .flatten()
                            .map(|bytes| Session::decode(&bytes))
                            .unwrap_or_default();
                        this.restore_session(session, window, cx);
                        // After the open notes have been asked for: storage works through its
                        // jobs in order, and the check and tidy-up can take a while in a big
                        // vault. The notes should not wait behind them.
                        this.storage.run_maintenance();
                        cx.notify();
                    }
                    Err(error) => this.report_error("loading notes", &error, cx),
                })
                .ok();
            }
        });

        let failures = cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(event) = events_rx.next().await {
                this.update(cx, |this, cx| this.on_storage_event(event, cx))
                    .ok();
            }
        });

        let ticker = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let keep_going = this.update_in(cx, Workspace::tick).unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        });

        cx.set_global(EditorSettings::default());
        let keystrokes = cx.observe_keystrokes(|this, _, _, _| this.touch());
        // Escape closes whatever is open on top of the notes, before anything under it hears
        // the key: with a menu showing, Escape must not close the find bar behind it.
        let this = cx.weak_entity();
        let escape = cx.intercept_keystrokes(move |event, window, cx| {
            let key = &event.keystroke;
            let handled = this
                .update(cx, |this, cx| {
                    if key.key == "escape" && !key.modifiers.modified() {
                        this.dismiss_popup(window, cx)
                    } else {
                        this.overlay_takes_key(key, window, cx)
                    }
                })
                .unwrap_or(false);
            if handled {
                cx.stop_propagation();
            }
        });
        // Workspace shortcuts (Ctrl+N, Ctrl+L, …) need focus inside the workspace.
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        Self {
            app,
            storage,
            notes: Vec::new(),
            loaded: false,
            tabs: Vec::new(),
            active: None,
            layout: TabLayout::default(),
            restoring: TabLayout::default(),
            pending_split: None,
            tab_bar: TabBarState::default(),
            search,
            body_hits: HashMap::new(),
            search_task: None,
            pending_find: None,
            show_trash: false,
            sidebar: SidebarState::default(),
            last_time_refresh: Instant::now(),
            confirm_delete: None,
            save_error: None,
            fatal_error: None,
            expired_trash: Vec::new(),
            new_recovery_key: None,
            ai_dialog: None,
            chat: None,
            chat_alone: false,
            backups: backups::BackupState::default(),
            session_dirty: false,
            last_session_write: Instant::now(),
            saved_session: None,
            settings: AppSettings::default(),
            show_settings: false,
            press_on_settings: false,
            settings_from_sidebar: false,
            last_activity: Instant::now(),
            last_activity_wall: SystemTime::now(),
            last_tick_wall: SystemTime::now(),
            last_session_check: Instant::now(),
            session_locked: None,
            spell_generation: 0,
            focus_handle,
            _tasks: vec![load, failures, ticker],
            _keystrokes: [keystrokes, escape],
        }
    }

    /// Closes the menu, popup or dialog that is open on top of the notes, if there is one.
    fn dismiss_popup(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.ai_dialog.is_some() {
            self.close_ai_dialog(window, cx);
        } else if self.dismiss_tab_menu() {
            // The tab menu was open and is closed now.
        } else if self.sidebar.note_menu.is_some() || self.sidebar.section_menu.is_some() {
            self.sidebar.note_menu = None;
            self.sidebar.section_menu = None;
        } else if self.show_settings {
            self.show_settings = false;
        } else if self.sidebar.section_field.take().is_some() {
            // A section name being typed is dropped, as Escape does in any rename field.
            self.focus_main(window, cx);
        } else {
            let tabs = self.tabs.clone();
            return tabs
                .iter()
                .any(|tab| tab.update(cx, |note, cx| note.dismiss_menu(window, cx)));
        }
        // Opening a menu with the mouse takes the keyboard away from the note (the click lands
        // on the tab bar or sidebar). Closing it with Escape gives it back.
        if self.focus_handle.is_focused(window) {
            self.focus_main(window, cx);
        }
        cx.notify();
        true
    }

    /// What is open on top of the notes, as far as the keyboard is concerned.
    fn overlay(&self, cx: &App) -> Overlay {
        if self.ai_dialog.is_some() {
            return Overlay::Dialog;
        }
        if self.new_recovery_key.is_some()
            || self.tab_bar_menu_open()
            || self.sidebar.note_menu.is_some()
            || self.sidebar.section_menu.is_some()
            || self.show_settings
        {
            return Overlay::Menu;
        }
        // Only notes on screen count: a menu that cannot be seen must never take the keyboard.
        let active = self.active_id(cx);
        let on_screen = |id: NoteId| {
            active.is_some_and(|active| {
                active == id
                    || self
                        .layout
                        .split_of(active)
                        .is_some_and(|split| split.has(id))
            })
        };
        let mut overlay = Overlay::None;
        for tab in &self.tabs {
            let note = tab.read(cx);
            if !on_screen(note.id) {
                continue;
            }
            if note.menu_open() {
                return Overlay::Menu;
            }
            if note.link_popup_open() {
                overlay = Overlay::Dialog;
            }
        }
        overlay
    }

    /// Keeps a key away from what is under a menu or dialog. With a menu open, typing must not
    /// change the note behind it; with a dialog open, its fields work but shortcuts such as
    /// Ctrl+N or Ctrl+W do not reach the workspace. Returns whether the key stops here.
    fn overlay_takes_key(
        &mut self,
        key: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match self.overlay(cx) {
            Overlay::None => false,
            // Escape still reaches the picture viewer, which closes on it.
            Overlay::Menu => key.key != "escape",
            Overlay::Dialog => {
                if key.key == "tab" {
                    // Tab stays inside the dialog.
                    if let Some(dialog) = &self.ai_dialog {
                        dialog.focus_other_field(window, cx);
                    }
                    return true;
                }
                let modifiers = key.modifiers;
                let shortcut = modifiers.control || modifiers.alt || modifiers.platform;
                shortcut
                    && !(modifiers.control
                        && !modifiers.alt
                        && EDITING_KEYS.contains(&key.key.as_str()))
            }
        }
    }

    /// Gives the keyboard to the note on screen, or to the workspace when there is none.
    fn focus_main(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active.and_then(|active| self.tabs.get(active)) {
            Some(tab) => tab.update(cx, |note, cx| note.focus(window, cx)),
            None => window.focus(&self.focus_handle, cx),
        }
    }

    fn on_storage_event(&mut self, event: StorageEvent, cx: &mut Context<Self>) {
        match event {
            StorageEvent::NoteSaved(id) => {
                if let Some(tab) = self.tabs.iter().find(|t| t.read(cx).id == id) {
                    tab.update(cx, |note, _| note.save_confirmed());
                }
            }
            StorageEvent::WriteFailed { what, note, error } => {
                if let Some(id) = note
                    && let Some(tab) = self.tabs.iter().find(|t| t.read(cx).id == id)
                {
                    tab.update(cx, |note, _| note.save_failed());
                }
                self.report_error(what, &error, cx);
            }
            StorageEvent::IntegrityProblem => {
                self.save_error = Some(
                    format!(
                        "The database check found a problem. Your notes are still open; recent \
                         backups are in {}.",
                        self.storage_backup_dir()
                    )
                    .into(),
                );
            }
            StorageEvent::BackedUp {
                newest_ms,
                copy_error,
            } => self.on_backed_up(newest_ms, copy_error),
            StorageEvent::Stopped => {
                self.fatal_error = Some(
                    "Saving has stopped because of an internal error. Copy any unsaved text to a \
                     safe place, then restart Inkveil."
                        .into(),
                );
            }
        }
        cx.notify();
    }

    /// Searches the text of all notes shortly after the search field stops changing.
    fn schedule_body_search(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).text().trim().to_owned();
        if query.chars().count() < 2 {
            self.body_hits.clear();
            self.search_task = None;
            return;
        }
        let storage = self.storage.clone();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            let hits = storage.search_bodies(query.clone()).await;
            this.update(cx, |this, cx| {
                if this.search.read(cx).text().trim() != query {
                    return;
                }
                let mut hits: HashMap<NoteId, BodyHit> =
                    hits.into_iter().map(|h| (h.id, h)).collect();
                // Open notes may have edits that are not saved yet.
                for tab in &this.tabs {
                    let note = tab.read(cx);
                    match crate::search::search_doc(note.id, note.doc(cx), &query) {
                        Some(hit) => hits.insert(note.id, hit),
                        None => hits.remove(&note.id),
                    };
                }
                this.body_hits = hits;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Opens a note from a search result and shows the search text in it.
    pub(super) fn open_note_and_find(
        &mut self,
        id: NoteId,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.tabs.iter().position(|t| t.read(cx).id == id) {
            self.activate(index, window, cx);
            self.tabs[index].update(cx, |note, cx| note.find_text(&query, window, cx));
        } else {
            self.pending_find = Some((id, query));
            self.open_note(id, None, true, window, cx);
        }
    }

    fn storage_backup_dir(&self) -> String {
        crate::vault_dir().join("backups").display().to_string()
    }

    fn create_recovery_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_settings = false;
        let storage = self.storage.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = storage.create_recovery_key().await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(key) => this.new_recovery_key = Some(key),
                    Err(error) => this.report_error("creating a recovery key", &error, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn report_error(&mut self, what: &str, error: &StoreError, cx: &mut Context<Self>) {
        self.save_error =
            Some(format!("Problem {what}: {error}. Your other notes are safe.").into());
        cx.notify();
    }

    /// Records keyboard or mouse input, which postpones auto-lock.
    fn touch(&mut self) {
        self.last_activity = Instant::now();
        self.last_activity_wall = SystemTime::now();
    }

    /// Runs every tick: autosave, session save, spelling refresh and auto-lock.
    /// Returns false once the vault is locking.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.autosave(false, cx);
        for tab in &self.tabs {
            tab.update(cx, NoteEditor::refresh_word_count);
        }
        // Keep "5 min ago" style times current.
        if self.last_time_refresh.elapsed() >= Duration::from_secs(30) {
            self.last_time_refresh = Instant::now();
            cx.notify();
        }
        let generation = spell::generation();
        if generation != self.spell_generation {
            self.spell_generation = generation;
            for tab in &self.tabs {
                tab.update(cx, |note, cx| note.refresh_spelling(cx));
            }
        }
        if self.loaded {
            self.save_session(false, cx);
        }
        self.backup_tick();
        self.clear_finished_drag(cx);
        if let Some((_, at)) = self.confirm_delete
            && at.elapsed() > Duration::from_secs(4)
        {
            self.confirm_delete = None;
            cx.notify();
        }

        // Wall-clock time keeps running while the PC sleeps; a big gap between ticks means it
        // was asleep. Idle time takes the larger of both clocks for the same reason.
        let now_wall = SystemTime::now();
        let tick_gap = now_wall
            .duration_since(self.last_tick_wall)
            .unwrap_or_default();
        self.last_tick_wall = now_wall;
        if self.last_session_check.elapsed() >= Duration::from_secs(1) {
            self.last_session_check = Instant::now();
            self.session_locked = inkveil_platform::is_session_locked();
        }
        let idle = self.last_activity.elapsed().max(
            now_wall
                .duration_since(self.last_activity_wall)
                .unwrap_or_default(),
        );
        if let Some(reason) = should_lock(&self.settings, idle, tick_gap, self.session_locked) {
            tracing::info!("auto-lock: {reason:?}");
            self.lock(window, cx);
            return false;
        }
        true
    }

    fn update_settings(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut AppSettings)) {
        f(&mut self.settings);
        cx.set_global(self.settings.editor);
        self.storage
            .put_state(state_keys::SETTINGS, self.settings.encode());
        cx.refresh_windows();
    }

    fn save_dictionary(&self) {
        let words = spell::personal_words().join("\n");
        self.storage.put_state(
            state_keys::DICTIONARY,
            zeroize::Zeroizing::new(words.into_bytes()),
        );
    }

    /// Hands due saves to storage. `force` saves every unsaved change now.
    fn autosave(&mut self, force: bool, cx: &mut Context<Self>) {
        let mut saves = Vec::new();
        for tab in &self.tabs {
            if let Some(save) = tab.update(cx, |note, cx| note.take_save(force, cx)) {
                saves.push(save);
            }
        }
        for tab in &self.tabs {
            let changed = tab.update(cx, |note, _| note.take_preview_changed());
            if changed {
                tab.update(cx, |note, cx| note.sync_title_placeholder(cx));
                cx.notify();
            }
        }
        for save in saves {
            let (id, snapshot) = (save.id, save.snapshot);
            self.queue_save(save);
            if snapshot {
                self.storage.snapshot(id);
            }
        }
    }

    /// Hands a note to storage and shows its new title, preview and time in the sidebar.
    fn queue_save(&mut self, save: note_editor::PendingSave) {
        if let Some(summary) = self.notes.iter_mut().find(|n| n.id == save.id) {
            summary.meta = save.meta.clone();
        }
        self.storage.save_note(save.id, save.meta, save.body);
    }

    /// Stores open tabs and carets if they changed since the last save. A change of tabs or
    /// their arrangement is stored at once. A caret that only moved waits up to
    /// [`CARET_SAVE_EVERY`], unless `now` is set: every write is a full commit to disk, and the
    /// caret moves with every key.
    fn save_session(&mut self, now: bool, cx: &App) {
        self.session_dirty = false;
        let session = Session {
            tabs: self
                .tabs
                .iter()
                .map(|tab| {
                    let note = tab.read(cx);
                    let caret = note.caret(cx);
                    OpenTab {
                        note: note.id,
                        caret_block: u32::try_from(caret.loc.block).unwrap_or(0),
                        caret_offset: u32::try_from(caret.offset).unwrap_or(0),
                    }
                })
                .collect(),
            active: self.active.and_then(|a| u32::try_from(a).ok()),
            pinned: self.layout.pinned.clone(),
            splits: self.layout.splits.clone(),
        };
        let Some(saved) = self.saved_session.as_ref() else {
            // Nothing stored since unlocking: what is on disk is at most as old as this.
            self.store_session(session);
            return;
        };
        if *saved == session {
            return;
        }
        if now
            || !saved.same_tabs(&session)
            || self.last_session_write.elapsed() >= CARET_SAVE_EVERY
        {
            self.store_session(session);
        }
    }

    fn store_session(&mut self, session: Session) {
        self.storage
            .put_state(state_keys::SESSION, session.encode());
        self.saved_session = Some(session);
        self.last_session_write = Instant::now();
    }

    fn restore_session(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        let active = session.active.map(|a| a as usize);
        let available = |id: NoteId| {
            self.notes
                .iter()
                .any(|n| n.id == id && n.meta.trashed_ms.is_none())
        };
        self.restoring = TabLayout {
            pinned: session
                .pinned
                .iter()
                .copied()
                .filter(|id| available(*id))
                .collect(),
            splits: session
                .splits
                .iter()
                .copied()
                .filter(|s| available(s.first) && available(s.second))
                .collect(),
        };
        for (index, tab) in session.tabs.into_iter().enumerate() {
            let Some(summary) = self
                .notes
                .iter()
                .find(|n| n.id == tab.note && n.meta.trashed_ms.is_none())
            else {
                continue;
            };
            let caret = Pos::new(tab.caret_block as usize, tab.caret_offset as usize);
            self.open_note(summary.id, Some(caret), active == Some(index), window, cx);
        }
    }

    /// Finds notes that have been in the trash longer than [`TRASH_DAYS`] and asks before
    /// deleting them. Nothing is deleted automatically: a wrong system clock must never cost notes.
    fn purge_old_trash(&mut self) {
        let Some(now) = inkveil_store::now_ms_checked() else {
            return;
        };
        let cutoff = now - TRASH_DAYS * 24 * 60 * 60 * 1000;
        self.expired_trash = self
            .notes
            .iter()
            .filter(|n| n.meta.trashed_ms.is_some_and(|t| t < cutoff))
            .map(|n| n.id)
            .collect();
    }

    fn delete_expired_trash(&mut self, cx: &mut Context<Self>) {
        let doomed = std::mem::take(&mut self.expired_trash);
        self.notes.retain(|n| !doomed.contains(&n.id));
        for id in doomed {
            self.storage.delete_permanently(id);
        }
        cx.notify();
    }

    // ----- Notes and tabs ----------------------------------------------------------------------

    fn new_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_note_in(None, window, cx);
    }

    /// Creates an empty note, filed under `section` if given, and opens it.
    fn new_note_in(
        &mut self,
        section: Option<inkveil_store::SectionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(id) = NoteId::generate() else {
            return;
        };
        let meta = NoteMeta {
            section,
            ..NoteMeta::new("")
        };
        let doc = Document::new();
        self.storage.save_note(id, meta.clone(), doc.encode());
        self.notes.push(NoteSummary {
            id,
            meta: meta.clone(),
        });
        self.show_trash = false;
        let tab = self.make_tab(id, meta, doc, window, cx);
        self.tabs.push(tab);
        self.activate(self.tabs.len() - 1, window, cx);
    }

    fn make_tab(
        &mut self,
        id: NoteId,
        meta: NoteMeta,
        doc: Document,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<NoteEditor> {
        let storage = self.storage.clone();
        let tab = cx.new(|cx| NoteEditor::new(id, meta, doc, storage, window, cx));
        cx.subscribe_in(&tab, window, |this, tab, event, window, cx| match event {
            NoteEditorEvent::MetaChanged => {
                let note = tab.read(cx);
                if let Some(summary) = this.notes.iter_mut().find(|n| n.id == note.id) {
                    summary.meta = note.meta().clone();
                }
                cx.notify();
            }
            NoteEditorEvent::Dirty => {}
            NoteEditorEvent::Problem(message) => {
                this.save_error = Some(message.clone());
                cx.notify();
            }
            NoteEditorEvent::DictionaryChanged => this.save_dictionary(),
            NoteEditorEvent::AiSettingsRequested => {
                let note = crate::ai::AiConfig::get(cx)
                    .is_none()
                    .then_some("Add your OpenRouter API key to use Improve with AI.");
                this.open_ai_settings(note, window, cx);
            }
            NoteEditorEvent::ChatToggled => this.toggle_chat(Some(tab.clone()), window, cx),
            NoteEditorEvent::TrashRequested => {
                let id = tab.read(cx).id;
                this.trash_note(id, window, cx);
            }
        })
        .detach();
        tab
    }

    /// Opens a note in a tab (or switches to its tab if already open).
    fn open_note(
        &mut self,
        id: NoteId,
        caret: Option<Pos>,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.tabs.iter().position(|t| t.read(cx).id == id) {
            if activate {
                self.activate(index, window, cx);
            }
            return;
        }
        let Some(meta) = self
            .notes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.meta.clone())
        else {
            return;
        };
        let storage = self.storage.clone();
        cx.spawn_in(window, async move |this, cx| {
            let body = storage.load_body(id).await;
            this.update_in(cx, |this, window, cx| {
                let doc = match body.map(|b| Document::decode(&b)) {
                    Ok(Ok(doc)) => doc,
                    Ok(Err(error)) => {
                        this.save_error =
                            Some(format!("This note could not be read: {error}").into());
                        cx.notify();
                        return;
                    }
                    Err(error) => return this.report_error("opening a note", &error, cx),
                };
                if this.tabs.iter().any(|t| t.read(cx).id == id) {
                    return;
                }
                let tab = this.make_tab(id, meta, doc, window, cx);
                if let Some(caret) = caret {
                    tab.update(cx, |note, cx| note.set_caret(caret, cx));
                }
                this.tabs.push(tab.clone());
                if activate || this.active.is_none() {
                    this.activate(this.tabs.len() - 1, window, cx);
                }
                if let Some((_, query)) = this.pending_find.take_if(|(p, _)| *p == id) {
                    tab.update(cx, |note, cx| note.find_text(&query, window, cx));
                }
                this.apply_restored_layout(cx);
                this.finish_open_in_split(id, window, cx);
                this.session_dirty = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = Some(index);
        self.session_dirty = true;
        self.tabs[index].update(cx, |note, cx| note.focus(window, cx));
        cx.notify();
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        // Save before the tab (and its unsaved state) goes away.
        if let Some(save) = self.tabs[index].update(cx, |note, cx| note.take_save(true, cx)) {
            let id = save.id;
            self.queue_save(save);
            self.storage.snapshot(id);
        }
        let id = self.tabs[index].read(cx).id;
        let partner = self.layout.split_of(id).map(|s| s.other(id));
        let was_active = self.active == Some(index);
        self.forget_tab(id);
        self.tabs.remove(index);
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(a) if a > index => Some(a - 1),
            Some(a) => Some(a.min(self.tabs.len() - 1)),
            None => None,
        };
        // Closing one side of a split leaves the other side on screen.
        if was_active && let Some(index) = partner.and_then(|p| self.tab_index(p, cx)) {
            self.active = Some(index);
        }
        if let Some(a) = self.active {
            self.tabs[a].update(cx, |note, cx| note.focus(window, cx));
        } else {
            window.focus(&self.focus_handle, cx);
        }
        self.session_dirty = true;
        cx.notify();
    }

    fn trash_note(&mut self, id: NoteId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.tabs.iter().position(|t| t.read(cx).id == id) {
            self.close_tab(index, window, cx);
        }
        if let Some(summary) = self.notes.iter_mut().find(|n| n.id == id) {
            summary.meta.trashed_ms = Some(now_ms());
            self.storage.save_meta(id, summary.meta.clone());
        }
        cx.notify();
    }

    fn restore_note(&mut self, id: NoteId, cx: &mut Context<Self>) {
        if let Some(summary) = self.notes.iter_mut().find(|n| n.id == id) {
            summary.meta.trashed_ms = None;
            self.storage.save_meta(id, summary.meta.clone());
        }
        cx.notify();
    }

    fn delete_forever(&mut self, id: NoteId, cx: &mut Context<Self>) {
        if self
            .confirm_delete
            .is_some_and(|(pending, _)| pending == id)
        {
            self.notes.retain(|n| n.id != id);
            self.storage.delete_permanently(id);
            self.confirm_delete = None;
        } else {
            self.confirm_delete = Some((id, Instant::now()));
        }
        cx.notify();
    }

    // ----- Locking -----------------------------------------------------------------------------

    fn lock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app = self.app.clone();
        window.defer(cx, move |window, cx| {
            app.update(cx, |app, cx| app.lock(window, cx)).ok();
        });
    }

    /// Queues every unsaved change and the session, and returns storage for locking.
    pub fn prepare_lock(&mut self, cx: &mut Context<Self>) -> Storage {
        self.autosave(true, cx);
        self.save_session(true, cx);
        RichClipboard::clear(cx);
        crate::clipboard::clear_ours();
        spell::clear_personal();
        self.forget_chat(cx);
        cx.set_global(crate::ai::AiConfig::default());
        crate::storage::set_active(None);
        self.storage.clone()
    }

    /// Saves everything synchronously; used when the app is quitting. Returns whether every
    /// change reached the disk.
    pub fn flush_blocking(&mut self, cx: &mut Context<Self>) -> bool {
        let failed = self.storage.failed_saves();
        self.autosave(true, cx);
        self.save_session(true, cx);
        crate::clipboard::clear_ours();
        let idle = self.storage.wait_idle_blocking(FLUSH_TIMEOUT);
        if !idle {
            tracing::error!("pending saves did not finish before exit");
        }
        idle && self.storage.failed_saves() == failed
    }

    /// Tells the user that closing was held back because something is not saved.
    pub fn report_unsaved_at_close(&mut self, cx: &mut Context<Self>) {
        self.storage.resume();
        self.save_error = Some(
            "Some changes could not be saved (the disk may be full, or another program is \
             using the vault), so Inkveil stayed open. Copy what you need somewhere safe, or \
             try again in a moment. Closing again quits anyway."
                .into(),
        );
        cx.notify();
    }

    // ----- Actions -----------------------------------------------------------------------------

    fn on_new_note(&mut self, _: &NewNote, window: &mut Window, cx: &mut Context<Self>) {
        self.new_note(window, cx);
    }

    fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(a) = self.active {
            self.close_tab(a, window, cx);
        }
    }

    fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(a) = self.active {
            self.activate((a + 1) % self.tabs.len(), window, cx);
        }
    }

    fn on_prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(a) = self.active {
            self.activate((a + self.tabs.len() - 1) % self.tabs.len(), window, cx);
        }
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.search.focus_handle(cx), cx);
        // What was searched for last is selected, so typing starts a new search.
        self.search.update(cx, TextField::select_everything);
    }

    fn on_lock(&mut self, _: &LockVault, window: &mut Window, cx: &mut Context<Self>) {
        self.lock(window, cx);
    }

    fn on_trash(&mut self, _: &TrashNote, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(a) = self.active {
            let id = self.tabs[a].read(cx).id;
            self.trash_note(id, window, cx);
        }
    }

    // ----- Rendering ---------------------------------------------------------------------------

    fn double_click_new_note(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.touch();
        if event.click_count == 2 {
            self.new_note(window, cx);
        }
    }

    fn render_settings_menu(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let colors = theme(cx);
        let row = |id: SharedString, label: String, checked: bool| {
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
                        .w(px(14.))
                        .text_color(colors.accent)
                        .child(if checked { "✓" } else { "" }),
                )
                .child(label)
        };
        let heading = |text: &'static str| {
            div()
                .px_2()
                .pt_2()
                .pb_1()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(colors.muted)
                .child(text)
        };
        let current = self.settings.auto_lock;
        let mut panel = div()
            .id("settings-menu")
            .debug_selector(|| "settings-menu".to_owned())
            .occlude()
            .flex()
            .flex_col()
            .p_1()
            .w(px(330.))
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .shadow_md()
            .text_size(px(13.))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if !this.press_on_settings {
                    this.show_settings = false;
                    cx.notify();
                }
            }))
            .child(heading("LOCK AUTOMATICALLY AFTER NO ACTIVITY"));
        for choice in AutoLock::CHOICES {
            let label = if choice == AutoLock::Never {
                "Never (only when I lock, close the app or restart)".to_owned()
            } else {
                choice.label()
            };
            panel = panel.child(
                row(
                    format!("lock-{}", choice.label()).into(),
                    label,
                    choice == current,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update_settings(cx, |s| s.auto_lock = choice);
                })),
            );
        }
        panel
            .child(div().h(px(1.)).my_1().bg(colors.border))
            .child(
                row(
                    "lock-with-system".into(),
                    "Lock when Windows locks or the PC sleeps".into(),
                    self.settings.lock_with_system,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.update_settings(cx, |s| s.lock_with_system = !s.lock_with_system);
                })),
            )
            .child(
                row(
                    "new-recovery-key".into(),
                    "Create a new recovery key…".into(),
                    false,
                )
                .on_click(cx.listener(|this, _, window, cx| this.create_recovery_key(window, cx))),
            )
            .child(div().h(px(1.)).my_1().bg(colors.border))
            .child(heading("BACKUPS"))
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .child(
                        "Encrypted: only your password or recovery key opens them. A copy on \
                         another drive or in a synced folder survives if this PC fails.",
                    ),
            )
            .child(
                row(
                    "backup-folder".into(),
                    match &self.backups.folder {
                        Some(folder) => format!("Copying to {}…", backups::short_path(folder)),
                        None => "Also copy daily backups to a folder…".to_owned(),
                    },
                    self.backups.folder.is_some(),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.choose_backup_folder(window, cx);
                })),
            )
            .when(self.backups.folder.is_some(), |d| {
                d.child(
                    row(
                        "open-backup-folder".into(),
                        "Open the backup folder".into(),
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.open_backup_folder(cx))),
                )
                .child(
                    row(
                        "stop-backup-copies".into(),
                        "Stop copying there (copies stay)".into(),
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.stop_backup_copies(cx))),
                )
            })
            .child(
                row("back-up-now".into(), "Back up now".into(), false)
                    .on_click(cx.listener(|this, _, _, cx| this.back_up_now(cx))),
            )
            .child({
                let (status, problem) = self.backup_status();
                div()
                    .px_2()
                    .pb_1()
                    .text_size(px(12.))
                    .text_color(if problem { colors.error } else { colors.muted })
                    .child(status)
            })
            .child(div().h(px(1.)).my_1().bg(colors.border))
            .child(heading("AI WRITING HELP"))
            .child(
                row(
                    "ai-key".into(),
                    match crate::ai::AiConfig::get(cx) {
                        Some(c) => format!("OpenRouter API key (…{})…", c.key_hint()),
                        None => "Set up OpenRouter API key…".to_owned(),
                    },
                    false,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_ai_settings(None, window, cx);
                })),
            )
            .child(div().h(px(1.)).my_1().bg(colors.border))
            .child(heading("EDITOR"))
            .child(
                row(
                    "wrap".into(),
                    "Word wrap (Alt+Z)".into(),
                    self.settings.editor.wrap,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.update_settings(cx, |s| s.editor.wrap = !s.editor.wrap);
                })),
            )
            .child(
                row(
                    "line-numbers".into(),
                    "Line numbers (Alt+N)".into(),
                    self.settings.editor.line_numbers,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.update_settings(cx, |s| s.editor.line_numbers = !s.editor.line_numbers);
                })),
            )
            .child(
                row(
                    "spellcheck".into(),
                    "Spellcheck (US English)".into(),
                    self.settings.editor.spellcheck,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.update_settings(cx, |s| s.editor.spellcheck = !s.editor.spellcheck);
                })),
            )
            .into_any_element()
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        let active = self.active.map(|a| self.tabs[a].read(cx));
        let words = active.map(NoteEditor::word_count);
        let line_col = active.map(|n| n.line_col(cx));
        let saving = active.is_some_and(NoteEditor::has_unsaved_changes);
        let has_active = active.is_some();
        let auto_lock = match self.settings.auto_lock {
            AutoLock::Never => "Auto-lock: never".to_owned(),
            other @ AutoLock::Minutes(_) => format!("Auto-lock: {}", other.label()),
        };
        let show_settings = self.show_settings;
        let status_item = |id: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_1()
                .px_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(|d| d.bg(colors.hover).text_color(colors.text))
        };
        let hidden = crate::capture_protected();
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .h(px(26.))
            .px_3()
            .flex_none()
            // In a narrow window the left side is cut short; nothing wraps or overlaps.
            .overflow_hidden()
            .whitespace_nowrap()
            .bg(colors.surface)
            .border_t_1()
            .border_color(colors.border)
            .text_size(px(12.))
            .text_color(colors.muted)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .id("lock")
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .hover(|d| d.text_color(colors.text))
                            .child(icon(Icon::Lock, colors.muted).size(px(13.)))
                            .child("Lock (Ctrl+L)")
                            .on_click(cx.listener(|this, _, window, cx| this.lock(window, cx))),
                    )
                    .child(
                        div()
                            .relative()
                            .child(
                                status_item("settings")
                                    .capture_any_mouse_down(cx.listener(|this, _, _, _| {
                                        this.press_on_settings = true;
                                    }))
                                    .when(show_settings && !self.settings_from_sidebar, |d| {
                                        d.bg(colors.hover)
                                    })
                                    .child(icon(Icon::Settings, colors.muted).size(px(12.)))
                                    .child("Settings")
                                    .child(
                                        div()
                                            .text_color(colors.muted)
                                            .child(format!("· {auto_lock}")),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_settings =
                                            !this.show_settings || this.settings_from_sidebar;
                                        this.settings_from_sidebar = false;
                                        cx.notify();
                                    })),
                            )
                            .when(show_settings && !self.settings_from_sidebar, |d| {
                                d.child(
                                    div().absolute().top_0().left_0().child(
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
                    .child(if hidden {
                        "Hidden from screen capture"
                    } else {
                        "Visible to screen capture on this system"
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .children(line_col.map(|(l, c)| format!("Ln {l}, Col {c}")))
                    .children(words.map(|w| format!("{w} words")))
                    .child(
                        status_item("toggle-wrap")
                            .child(if self.settings.editor.wrap {
                                "Wrap: on"
                            } else {
                                "Wrap: off"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_settings(cx, |s| s.editor.wrap = !s.editor.wrap);
                            })),
                    )
                    .child(
                        status_item("toggle-spelling")
                            .child(if self.settings.editor.spellcheck {
                                "Spelling: on"
                            } else {
                                "Spelling: off"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_settings(cx, |s| {
                                    s.editor.spellcheck = !s.editor.spellcheck;
                                });
                            })),
                    )
                    .child(
                        status_item("toggle-numbers")
                            .child(if self.settings.editor.line_numbers {
                                "Line numbers: on"
                            } else {
                                "Line numbers: off"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_settings(cx, |s| {
                                    s.editor.line_numbers = !s.editor.line_numbers;
                                });
                            })),
                    )
                    .when(has_active, |d| {
                        d.child(if saving { "Saving…" } else { "Saved" })
                    }),
            )
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        let main: gpui::AnyElement = match (self.render_notes(cx), self.chat.clone()) {
            (Some(notes), _) => notes,
            (None, Some(chat)) if self.chat_alone => chat.into_any_element(),
            (None, _) => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(colors.muted)
                .child(
                    gpui::img("images/inkveil.png")
                        .size(px(64.))
                        .rounded_lg()
                        .opacity(0.85),
                )
                .child("Select a note, or press Ctrl+N to start a new one.")
                .child("Press Ctrl+J to chat with Grok.")
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            // Runs before the listeners of anything inside: each press starts afresh.
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.press_on_settings = false))
            .on_action(cx.listener(Self::on_new_note))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(|this, _: &ReopenClosedTab, window, cx| {
                this.reopen_closed_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSplit, window, cx| {
                this.toggle_split(window, cx);
            }))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(|this, _: &SaveNow, _, cx| this.autosave(true, cx)))
            .on_action(cx.listener(Self::on_lock))
            .on_action(cx.listener(|this, _: &ToggleChat, window, cx| {
                let tab = this.active.map(|active| this.tabs[active].clone());
                this.toggle_chat(tab, window, cx);
            }))
            .on_action(cx.listener(Self::on_trash))
            .on_action(cx.listener(|this, _: &ToggleWrap, _, cx| {
                this.update_settings(cx, |s| s.editor.wrap = !s.editor.wrap);
            }))
            .on_action(cx.listener(|this, _: &ToggleLineNumbers, _, cx| {
                this.update_settings(cx, |s| s.editor.line_numbers = !s.editor.line_numbers);
            }))
            .on_any_mouse_down(cx.listener(|this, _, _, _| this.touch()))
            .on_scroll_wheel(cx.listener(|this, _, _, _| this.touch()))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_sidebar(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(self.render_tabs(cx))
                            .children(self.fatal_error.clone().map(|error| {
                                div()
                                    .px_3()
                                    .py_2()
                                    .bg(colors.error)
                                    .text_color(gpui::white())
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(error)
                            }))
                            .when(!self.expired_trash.is_empty(), |d| {
                                d.child(self.render_expired_trash_banner(cx))
                            })
                            .children(self.save_error.clone().map(|error| {
                                div()
                                    .id("save-error")
                                    .px_3()
                                    .py_2()
                                    .bg(colors.error)
                                    .text_color(gpui::white())
                                    .text_size(px(12.))
                                    .cursor_pointer()
                                    .child(error)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_error = None;
                                        cx.notify();
                                    }))
                            }))
                            .child(div().flex_1().min_h_0().child(main)),
                    ),
            )
            .child(self.render_status(cx))
            .children(self.render_tab_menu(window, cx))
            .children(self.render_recovery_key_dialog(cx))
            .children(self.render_ai_dialog(cx))
    }
}

impl Workspace {
    fn render_expired_trash_banner(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        let count = self.expired_trash.len();
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .bg(colors.selected)
            .text_size(px(12.))
            .child(div().flex_1().child(format!(
                "{count} note(s) have been in the trash for more than {TRASH_DAYS} days. Delete them for good?"
            )))
            .child(
                crate::ui::button("delete-expired", "Delete", false, cx)
                    .h(px(26.))
                    .on_click(cx.listener(|this, _, _, cx| this.delete_expired_trash(cx))),
            )
            .child(
                crate::ui::button("keep-expired", "Keep for now", false, cx)
                    .h(px(26.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.expired_trash.clear();
                        cx.notify();
                    })),
            )
    }

    fn render_recovery_key_dialog(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let key = self.new_recovery_key.as_ref()?.to_string();
        let colors = theme(cx);
        Some(
            deferred(
                div()
                    .id("recovery-dialog-backdrop")
                    .occlude()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgba(0x0000_0088))
                    .child(
                        div()
                            .w(px(520.))
                            .p_6()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .rounded_lg()
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.background)
                            .shadow_lg()
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Your new recovery key"),
                            )
                            .child(div().text_color(colors.muted).child(
                                "The old recovery key no longer works. Write this one down and keep \
                                 it somewhere safe; it is shown only now. Copy puts it on the \
                                 clipboard for 2 minutes; paste with Ctrl+V (it is kept out of \
                                 clipboard history).",
                            ))
                            .child(
                                div()
                                    .p_4()
                                    .rounded_md()
                                    .bg(colors.code_background)
                                    .font_family(crate::theme::MONO_FONT)
                                    .text_size(px(16.))
                                    .text_center()
                                    .whitespace_nowrap()
                                    .child(key),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        crate::ui::button("copy-new-key", "Copy", false, cx)
                                            .flex_1()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                if let Some(key) = &this.new_recovery_key {
                                                    crate::clipboard::write_secret_text(key, cx);
                                                }
                                            })),
                                    )
                                    .child(
                                        crate::ui::button("new-key-done", "I saved it", true, cx)
                                            .flex_1()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.new_recovery_key = None;
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    ),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }
}
