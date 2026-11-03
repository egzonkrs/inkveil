//! The storage worker: one thread that owns the unlocked vault.
//!
//! The UI never touches the database. It sends jobs over a channel; the worker encrypts and
//! writes them in order, so saves can never overtake each other and the UI never waits on disk.
//! Every save is confirmed (or reported as failed) through [`StorageEvent`]s, so the UI only
//! shows "Saved" once a note really is on disk, and retries if it is not.

use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use futures::channel::{mpsc::UnboundedSender, oneshot};
use inkveil_store::{
    LockedVault, NoteId, NoteListing, NoteMeta, RevisionId, RevisionSummary, StoreError, Vault,
    now_ms,
};
use zeroize::Zeroizing;

/// Keys for the app state stored in the vault.
pub mod state_keys {
    /// Open tabs, active tab and caret positions.
    pub const SESSION: &str = "session";
    /// Auto-lock and editor settings.
    pub const SETTINGS: &str = "settings";
    /// Words added to the spellchecker.
    pub const DICTIONARY: &str = "dictionary";
    /// The OpenRouter API key and model for "Improve with AI".
    pub const AI: &str = "ai";
    /// Sidebar sections.
    pub const SECTIONS: &str = "sections";
    /// The folder daily backups are also copied to.
    pub const BACKUP_FOLDER: &str = "backup-folder";
}

/// Left in the backup folder, so the copies can be used without this README at hand.
const RESTORE_NOTE: &str = "\
These are encrypted backups of your Inkveil notes, made by Inkveil: the newest 7, then one a week
for 4 more weeks. Each vault-<number>.db file is a complete copy (the number is when it was made).
They open only with your Inkveil password or recovery key; without one of them they are useless
to anyone, including you.

To restore one:
1. Close Inkveil.
2. Open %LOCALAPPDATA%\\Inkveil\\vault and move everything in it to a safe place (do not delete it).
3. Copy the backup you want (usually the newest) into that folder and rename the copy to vault.db.
4. Start Inkveil and unlock it with your password or recovery key.
";

/// Make a backup (at unlock, and hourly checks while unlocked) if the newest one is older than this.
const BACKUP_EVERY_MS: i64 = 20 * 60 * 60 * 1000;

enum Job {
    SaveNote {
        id: NoteId,
        meta: NoteMeta,
        body: Zeroizing<Vec<u8>>,
    },
    SaveMeta {
        id: NoteId,
        meta: NoteMeta,
    },
    Snapshot {
        id: NoteId,
    },
    DeletePermanently {
        id: NoteId,
    },
    PutState {
        key: &'static str,
        value: Zeroizing<Vec<u8>>,
    },
    PutBlob {
        bytes: Vec<u8>,
        reply: oneshot::Sender<Result<inkveil_store::BlobId, StoreError>>,
    },
    LoadBlob {
        id: inkveil_store::BlobId,
        reply: oneshot::Sender<Result<Zeroizing<Vec<u8>>, StoreError>>,
    },
    SearchBodies {
        query: String,
        reply: oneshot::Sender<Vec<crate::search::BodyHit>>,
    },
    ListNotes {
        reply: oneshot::Sender<Result<NoteListing, StoreError>>,
    },
    LoadBody {
        id: NoteId,
        reply: oneshot::Sender<Result<Zeroizing<Vec<u8>>, StoreError>>,
    },
    GetState {
        key: &'static str,
        reply: oneshot::Sender<Result<Option<Zeroizing<Vec<u8>>>, StoreError>>,
    },
    ListRevisions {
        id: NoteId,
        reply: oneshot::Sender<Result<Vec<RevisionSummary>, StoreError>>,
    },
    LoadRevision {
        id: NoteId,
        revision: RevisionId,
        reply: oneshot::Sender<Result<Zeroizing<Vec<u8>>, StoreError>>,
    },
    /// Integrity check plus a backup if one is due.
    Maintenance,
    /// Where backups are also copied from now on.
    SetBackupFolder(Option<PathBuf>),
    /// A backup if one is due, or now with `force`, copied to the backup folder.
    Backup {
        force: bool,
    },
    CreateRecoveryKey {
        reply: oneshot::Sender<Result<Zeroizing<String>, StoreError>>,
    },
    Lock {
        reply: oneshot::Sender<LockedVault>,
    },
}

/// What the worker reports back to the UI.
#[derive(Debug)]
pub enum StorageEvent {
    /// A note's save reached the disk.
    NoteSaved(NoteId),
    /// A write failed. `note` is set when it was a note save, which the UI should retry.
    WriteFailed {
        what: &'static str,
        note: Option<NoteId>,
        error: StoreError,
    },
    /// The database structure check found a problem.
    IntegrityProblem,
    /// A backup round finished: when the newest backup was made, and why copying it to the
    /// backup folder failed, if it did.
    BackedUp {
        newest_ms: Option<i64>,
        copy_error: Option<StoreError>,
    },
    /// The worker stopped after an internal error; nothing more will be saved.
    Stopped,
}

/// Handle to the storage worker. Cheap to clone.
#[derive(Clone)]
pub struct Storage {
    jobs: mpsc::Sender<Job>,
    pending: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
    events: UnboundedSender<StorageEvent>,
}

/// The storage of the unlocked vault, for the panic hook.
static ACTIVE: Mutex<Option<Storage>> = Mutex::new(None);

/// Registers (or clears) the storage the panic hook should flush.
pub fn set_active(storage: Option<Storage>) {
    if let Ok(mut active) = ACTIVE.lock() {
        *active = storage;
    }
}

/// The storage of the unlocked vault, if any.
pub fn active() -> Option<Storage> {
    ACTIVE.lock().ok().and_then(|a| a.clone())
}

impl Storage {
    /// Starts the worker thread, which takes ownership of `vault`.
    pub fn start(vault: Vault, events: UnboundedSender<StorageEvent>) -> Self {
        let (jobs, rx) = mpsc::channel::<Job>();
        let pending = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            pending: pending.clone(),
            stopped: stopped.clone(),
            events: events.clone(),
            backup_folder: RefCell::new(None),
            copying: Arc::new(AtomicBool::new(false)),
        };
        thread::Builder::new()
            .name("inkveil-storage".into())
            .spawn(move || worker.run(vault, &rx))
            .expect("failed to start the storage thread");
        Self {
            jobs,
            pending,
            stopped,
            events,
        }
    }

    fn send(&self, job: Job) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        if self.jobs.send(job).is_err() {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            if !self.stopped.swap(true, Ordering::SeqCst) {
                let _ = self.events.unbounded_send(StorageEvent::Stopped);
            }
        }
    }

    /// Whether the worker has stopped (after an internal error).
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    pub fn save_note(&self, id: NoteId, meta: NoteMeta, body: Zeroizing<Vec<u8>>) {
        self.send(Job::SaveNote { id, meta, body });
    }

    pub fn save_meta(&self, id: NoteId, meta: NoteMeta) {
        self.send(Job::SaveMeta { id, meta });
    }

    pub fn snapshot(&self, id: NoteId) {
        self.send(Job::Snapshot { id });
    }

    pub fn delete_permanently(&self, id: NoteId) {
        self.send(Job::DeletePermanently { id });
    }

    pub fn put_state(&self, key: &'static str, value: Zeroizing<Vec<u8>>) {
        self.send(Job::PutState { key, value });
    }

    pub fn run_maintenance(&self) {
        self.send(Job::Maintenance);
    }

    /// Sets the folder backups are also copied to (`None`: only the vault's own backups).
    pub fn set_backup_folder(&self, folder: Option<PathBuf>) {
        self.send(Job::SetBackupFolder(folder));
    }

    /// Makes a backup if one is due (or now, with `force`) and copies it to the backup folder.
    pub fn back_up(&self, force: bool) {
        self.send(Job::Backup { force });
    }

    pub async fn list_notes(&self) -> Result<NoteListing, StoreError> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::ListNotes { reply });
        rx.await.unwrap_or_else(|_| Err(stopped_error()))
    }

    /// Stores a (cleaned-up) picture encrypted and returns its id.
    pub async fn put_blob(&self, bytes: Vec<u8>) -> Result<inkveil_store::BlobId, StoreError> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::PutBlob { bytes, reply });
        rx.await.unwrap_or_else(|_| Err(stopped_error()))
    }

    /// The decrypted bytes of a stored picture.
    pub async fn load_blob(
        &self,
        id: inkveil_store::BlobId,
    ) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::LoadBlob { id, reply });
        rx.await.unwrap_or_else(|_| Err(stopped_error()))
    }

    /// Notes whose text contains `query` (ignoring case), with a snippet around the first match.
    pub async fn search_bodies(&self, query: String) -> Vec<crate::search::BodyHit> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::SearchBodies { query, reply });
        rx.await.unwrap_or_default()
    }

    pub async fn load_body(&self, id: NoteId) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::LoadBody { id, reply });
        rx.await.unwrap_or_else(|_| Err(stopped_error()))
    }

    pub async fn get_state(
        &self,
        key: &'static str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::GetState { key, reply });
        rx.await.unwrap_or_else(|_| Err(stopped_error()))
    }

    /// A note's saved history, oldest first.
    pub async fn list_revisions(&self, id: NoteId) -> Result<Vec<RevisionSummary>, StoreError> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::ListRevisions { id, reply });
        rx.await.unwrap_or_else(|_| Err(stopped_error()))
    }

    /// The body of one saved version of a note.
    pub async fn load_revision(
        &self,
        id: NoteId,
        revision: RevisionId,
    ) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::LoadRevision {
            id,
            revision,
            reply,
        });
        rx.await.unwrap_or_else(|_| Err(stopped_error()))
    }

    /// Replaces the recovery key and returns the new one, formatted for display.
    pub async fn create_recovery_key(&self) -> Result<Zeroizing<String>, StoreError> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::CreateRecoveryKey { reply });
        rx.await.unwrap_or_else(|_| Err(stopped_error()))
    }

    /// Writes everything pending, wipes the keys and returns the locked vault.
    pub async fn lock(self) -> Option<LockedVault> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::Lock { reply });
        rx.await.ok()
    }

    /// Blocks the calling thread until pending writes finish or `timeout` passes.
    ///
    /// Used when the app is quitting or crashing, where nothing async can run any more.
    pub fn wait_idle_blocking(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while self.pending.load(Ordering::SeqCst) > 0 && !self.is_stopped() {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(5));
        }
        true
    }
}

fn stopped_error() -> StoreError {
    StoreError::Corrupt("the storage worker stopped")
}

struct Worker {
    pending: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
    events: UnboundedSender<StorageEvent>,
    backup_folder: RefCell<Option<PathBuf>>,
    /// A copy to the backup folder is running on its own thread.
    copying: Arc<AtomicBool>,
}

impl Worker {
    fn run(self, mut vault: Vault, rx: &mpsc::Receiver<Job>) {
        while let Ok(job) = rx.recv() {
            if let Job::Lock { reply } = job {
                self.pending.fetch_sub(1, Ordering::SeqCst);
                // Anything sent after the lock request belongs to a session that is ending.
                let _ = reply.send(vault.lock());
                return;
            }
            // A bug must not take saving down silently: stop and tell the UI instead.
            let outcome = catch_unwind(AssertUnwindSafe(|| self.handle(&mut vault, job)));
            self.pending.fetch_sub(1, Ordering::SeqCst);
            if outcome.is_err() {
                tracing::error!("storage worker panicked; stopping");
                self.stopped.store(true, Ordering::SeqCst);
                let _ = self.events.unbounded_send(StorageEvent::Stopped);
                return;
            }
        }
    }

    fn handle(&self, vault: &mut Vault, job: Job) {
        let report =
            |what: &'static str, note: Option<NoteId>, result: Result<(), StoreError>| match result
            {
                Ok(()) => {
                    if let Some(id) = note {
                        let _ = self.events.unbounded_send(StorageEvent::NoteSaved(id));
                    }
                }
                Err(error) => {
                    tracing::error!("{what} failed: {error}");
                    let _ =
                        self.events
                            .unbounded_send(StorageEvent::WriteFailed { what, note, error });
                }
            };
        match job {
            Job::SaveNote { id, meta, body } => {
                report(
                    "saving a note",
                    Some(id),
                    vault.save_note(&id, &meta, &body),
                );
            }
            Job::SaveMeta { id, meta } => {
                report("saving a note", None, vault.save_meta(&id, &meta));
            }
            Job::Snapshot { id } => {
                let result = vault
                    .snapshot_revision(&id)
                    .and_then(|_| vault.thin_revisions(&id, now_ms()))
                    .map(drop);
                report("saving note history", None, result);
            }
            Job::DeletePermanently { id } => {
                report("deleting a note", None, vault.delete_note_permanently(&id));
            }
            Job::PutState { key, value } => {
                report("saving app state", None, vault.put_state(key, &value));
            }
            Job::ListNotes { reply } => {
                let _ = reply.send(vault.notes());
            }
            Job::PutBlob { bytes, reply } => {
                let _ = reply.send(vault.put_blob(&bytes));
            }
            Job::LoadBlob { id, reply } => {
                let _ = reply.send(vault.load_blob(id));
            }
            Job::SearchBodies { query, reply } => {
                let hits = vault.notes().map_or_else(
                    |_| Vec::new(),
                    |listing| {
                        listing
                            .notes
                            .iter()
                            .filter_map(|note| {
                                let body = vault.load_body(&note.id).ok()?;
                                let doc = inkveil_doc::Document::decode(&body).ok()?;
                                crate::search::search_doc(note.id, &doc, &query)
                            })
                            .collect()
                    },
                );
                let _ = reply.send(hits);
            }
            Job::LoadBody { id, reply } => {
                let _ = reply.send(vault.load_body(&id));
            }
            Job::GetState { key, reply } => {
                let _ = reply.send(vault.get_state(key));
            }
            Job::ListRevisions { id, reply } => {
                let _ = reply.send(vault.revisions(&id));
            }
            Job::LoadRevision {
                id,
                revision,
                reply,
            } => {
                let _ = reply.send(vault.load_revision_body(&id, revision));
            }
            Job::Maintenance => {
                match vault.quick_check() {
                    Ok(true) => {}
                    Ok(false) | Err(_) => {
                        let _ = self.events.unbounded_send(StorageEvent::IntegrityProblem);
                    }
                }
                report(
                    "making a backup",
                    None,
                    vault.backup_if_due(BACKUP_EVERY_MS).map(drop),
                );
                self.copy_backups(vault);
                report("tidying up pictures", None, delete_unused_pictures(vault));
            }
            Job::SetBackupFolder(folder) => {
                *self.backup_folder.borrow_mut() = folder;
            }
            Job::Backup { force } => {
                // Pressing "Back up now" twice in a row makes one backup.
                let max_age = if force { 60 * 1000 } else { BACKUP_EVERY_MS };
                report(
                    "making a backup",
                    None,
                    vault.backup_if_due(max_age).map(drop),
                );
                self.copy_backups(vault);
            }
            Job::CreateRecoveryKey { reply } => {
                let _ = reply.send(vault.create_recovery_key().map(|k| k.display()));
            }
            Job::Lock { .. } => unreachable!("handled in run"),
        }
    }
}

impl Worker {
    /// Copies the newest backup to the backup folder, if one is set, and reports how it went.
    ///
    /// The copy runs on its own thread: a slow USB stick or a network folder that hangs must
    /// never hold up saving notes. It only reads finished backup files, never the database. A
    /// folder that cannot be reached is reported, not retried; the next round (hourly) copies
    /// whatever is newest then.
    fn copy_backups(&self, vault: &Vault) {
        let newest_ms = inkveil_store::maintenance::list_backups(vault.paths())
            .ok()
            .and_then(|backups| backups.first().map(|(_, at)| *at));
        let Some(folder) = self.backup_folder.borrow().clone() else {
            let _ = self.events.unbounded_send(StorageEvent::BackedUp {
                newest_ms,
                copy_error: None,
            });
            return;
        };
        if self.copying.swap(true, Ordering::SeqCst) {
            // The running copy reports when it is done.
            return;
        }
        let (paths, copying, events) = (
            vault.paths().clone(),
            self.copying.clone(),
            self.events.clone(),
        );
        let spawned = thread::Builder::new()
            .name("inkveil-backup-copy".into())
            .spawn(move || {
                let result = inkveil_store::maintenance::copy_backups_to(&paths, &folder);
                let note = folder.join("How to restore.txt");
                if result.is_ok() && !note.exists() {
                    let _ = std::fs::write(note, RESTORE_NOTE.replace('\n', "\r\n"));
                }
                if let Err(error) = &result {
                    tracing::warn!("copying the backup failed: {error}");
                }
                copying.store(false, Ordering::SeqCst);
                let _ = events.unbounded_send(StorageEvent::BackedUp {
                    newest_ms,
                    copy_error: result.err(),
                });
            });
        if let Err(error) = spawned {
            self.copying.store(false, Ordering::SeqCst);
            let _ = self.events.unbounded_send(StorageEvent::BackedUp {
                newest_ms,
                copy_error: Some(error.into()),
            });
        }
    }
}

/// Removes pictures that no note or saved version uses any more. Only pictures older than a day
/// are considered, so one pasted moments ago is never removed before its note is saved. If any
/// note cannot be read, nothing is removed: a picture it uses might be lost.
fn delete_unused_pictures(vault: &Vault) -> Result<(), StoreError> {
    use std::collections::HashSet;

    let mut in_use: HashSet<inkveil_store::BlobId> = HashSet::new();
    let mut collect = |body: &[u8]| -> bool {
        let Ok(doc) = inkveil_doc::Document::decode(body) else {
            return false;
        };
        for block in doc.blocks() {
            if let Some(image) = block.as_image() {
                in_use.insert(inkveil_store::BlobId(image.blob));
            }
        }
        true
    };
    let listing = vault.notes()?;
    if !listing.unreadable.is_empty() {
        return Ok(());
    }
    for note in &listing.notes {
        if !collect(&vault.load_body(&note.id)?) {
            return Ok(());
        }
        for revision in vault.revisions(&note.id)? {
            if !collect(&vault.load_revision_body(&note.id, revision.id)?) {
                return Ok(());
            }
        }
    }
    let removed = vault.delete_unused_blobs(&in_use, 24 * 60 * 60 * 1000)?;
    if removed > 0 {
        tracing::info!("removed {removed} unused picture(s)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use futures::StreamExt as _;
    use inkveil_crypto::{KdfParams, Password};
    use inkveil_store::VaultPaths;

    use super::*;

    #[test]
    fn backups_are_copied_to_the_chosen_folder() {
        let dir = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(dir.path().join("vault"));
        let vault =
            Vault::create(paths, &Password::new("correct horse 7"), KdfParams::TESTING).unwrap();
        let (events, mut rx) = futures::channel::mpsc::unbounded();
        let storage = Storage::start(vault, events);
        let folder = dir.path().join("usb").join("Inkveil backups");
        storage.set_backup_folder(Some(folder.clone()));
        storage.back_up(true);

        let (newest_ms, copy_error) = futures::executor::block_on(async {
            loop {
                if let Some(StorageEvent::BackedUp {
                    newest_ms,
                    copy_error,
                }) = rx.next().await
                {
                    return (newest_ms, copy_error);
                }
            }
        });
        assert!(copy_error.is_none(), "{copy_error:?}");
        let newest = newest_ms.unwrap();
        assert!(folder.join(format!("vault-{newest}.db")).is_file());
        assert!(folder.join("How to restore.txt").is_file());
        futures::executor::block_on(storage.lock()).unwrap();
    }
}
