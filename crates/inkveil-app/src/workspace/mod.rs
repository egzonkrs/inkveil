//! The unlocked app: sidebar with notes, tabs of open notes, the editor and a status bar.

mod ai_panel;
mod ai_settings;
mod backups;
mod find;
mod history;
mod note_editor;
mod pictures;
mod sections;
mod session;
mod sidebar;

use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime},
};

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, Render,
    SharedString, Task, WeakEntity, Window, actions, anchored, deferred, div, prelude::*, px,
};
use inkveil_doc::{Document, Pos};
use inkveil_store::{NoteId, NoteMeta, NoteSummary, StoreError, Vault, now_ms};

use self::{
    note_editor::{NoteEditor, NoteEditorEvent},
    session::{OpenTab, Session},
    sidebar::SidebarState,
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
        NextTab,
        PrevTab,
        FocusSearch,
        LockVault,
        TrashNote,
        ToggleWrap,
        ToggleLineNumbers
    ]
);

const CONTEXT: &str = "Workspace";
/// Trashed notes are deleted for good after this many days.
const TRASH_DAYS: i64 = 30;
const TICK: Duration = Duration::from_millis(250);

pub fn key_bindings() -> Vec<gpui::KeyBinding> {
    use gpui::KeyBinding;
    let ctx = Some(CONTEXT);
    let mut bindings = find::key_bindings();
    bindings.extend([
        KeyBinding::new("ctrl-n", NewNote, ctx),
        KeyBinding::new("ctrl-w", CloseTab, ctx),
        KeyBinding::new("ctrl-f4", CloseTab, ctx),
        KeyBinding::new("ctrl-tab", NextTab, ctx),
        KeyBinding::new("ctrl-pagedown", NextTab, ctx),
        KeyBinding::new("ctrl-shift-tab", PrevTab, ctx),
        KeyBinding::new("ctrl-pageup", PrevTab, ctx),
        KeyBinding::new("ctrl-p", FocusSearch, ctx),
        KeyBinding::new("ctrl-l", LockVault, ctx),
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
    backups: backups::BackupState,
    session_dirty: bool,
    saved_session: Option<Session>,
    settings: AppSettings,
    show_settings: bool,
    /// The settings menu was opened from the sidebar's gear (else from the status bar).
    settings_from_sidebar: bool,
    last_activity: Instant,
    last_activity_wall: SystemTime,
    last_tick_wall: SystemTime,
    last_session_check: Instant,
    session_locked: Option<bool>,
    spell_generation: u64,
    focus_handle: FocusHandle,
    _tasks: Vec<Task<()>>,
    _keystrokes: gpui::Subscription,
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
        cx.subscribe(&search, |this, _, event, cx| {
            if *event == crate::ui::text_field::TextFieldEvent::Changed {
                this.schedule_body_search(cx);
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
                        this.storage.run_maintenance();
                        this.loaded = true;
                        this.purge_old_trash();
                        let session = session
                            .ok()
                            .flatten()
                            .map(|bytes| Session::decode(&bytes))
                            .unwrap_or_default();
                        this.restore_session(session, window, cx);
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
            backups: backups::BackupState::default(),
            session_dirty: false,
            saved_session: None,
            settings: AppSettings::default(),
            show_settings: false,
            settings_from_sidebar: false,
            last_activity: Instant::now(),
            last_activity_wall: SystemTime::now(),
            last_tick_wall: SystemTime::now(),
            last_session_check: Instant::now(),
            session_locked: None,
            spell_generation: 0,
            focus_handle,
            _tasks: vec![load, failures, ticker],
            _keystrokes: keystrokes,
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
            self.save_session(cx);
        }
        self.backup_tick();
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
            if let Some(summary) = self.notes.iter_mut().find(|n| n.id == save.id) {
                summary.meta = save.meta.clone();
            }
            self.storage.save_note(save.id, save.meta, save.body);
            if save.snapshot {
                self.storage.snapshot(save.id);
            }
        }
    }

    /// Stores open tabs and carets if they changed since the last save.
    fn save_session(&mut self, cx: &App) {
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
        };
        if self.saved_session.as_ref() != Some(&session) {
            self.storage
                .put_state(state_keys::SESSION, session.encode());
            self.saved_session = Some(session);
        }
    }

    fn restore_session(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        let active = session.active.map(|a| a as usize);
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
            self.storage.save_note(save.id, save.meta, save.body);
            self.storage.snapshot(save.id);
        }
        self.tabs.remove(index);
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(a) if a > index => Some(a - 1),
            Some(a) => Some(a.min(self.tabs.len() - 1)),
            None => None,
        };
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
        self.save_session(cx);
        RichClipboard::clear(cx);
        crate::clipboard::clear_ours();
        spell::clear_personal();
        cx.set_global(crate::ai::AiConfig::default());
        crate::storage::set_active(None);
        self.storage.clone()
    }

    /// Saves everything synchronously; used when the app is quitting.
    pub fn flush_blocking(&mut self, cx: &mut Context<Self>) {
        self.autosave(true, cx);
        self.save_session(cx);
        crate::clipboard::clear_ours();
        if !self.storage.wait_idle_blocking(Duration::from_secs(5)) {
            tracing::error!("pending saves did not finish before exit");
        }
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

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
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
            .children(self.tabs.iter().enumerate().map(|(index, tab)| {
                let note = tab.read(cx);
                let active = self.active == Some(index);
                let title = note.display_title();
                div()
                    .id(("tab", index))
                    .group("tab")
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl_3()
                    .pr_1()
                    .max_w(px(220.))
                    .h_full()
                    .flex_none()
                    .border_r_1()
                    .border_color(colors.border)
                    .cursor_pointer()
                    .text_size(px(13.))
                    .when(active, |d| d.bg(colors.background).text_color(colors.text))
                    .when(!active, |d| {
                        d.text_color(colors.muted).hover(|d| d.bg(colors.hover))
                    })
                    .child(icon(Icon::FileText, colors.muted).size(px(14.)))
                    .child(div().truncate().child(title))
                    .child(
                        div()
                            .id(("close", index))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(20.))
                            .rounded_sm()
                            .when(!active, |d| {
                                d.invisible().group_hover("tab", gpui::Styled::visible)
                            })
                            .hover(|d| d.bg(colors.hover))
                            .child(icon(Icon::X, colors.muted).size(px(13.)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(index, window, cx);
                            })),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.activate(index, window, cx)),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| this.close_tab(index, window, cx)),
                    )
            }))
            .child(
                div()
                    .id("tabs-empty")
                    .flex_1()
                    .min_w(px(48.))
                    .h_full()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::double_click_new_note)),
            )
    }

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
                this.show_settings = false;
                cx.notify();
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
            .h(px(26.))
            .px_3()
            .flex_none()
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        let main: gpui::AnyElement = match self.active {
            Some(a) => self.tabs[a].clone().into_any_element(),
            None => div()
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
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_new_note))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_lock))
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
