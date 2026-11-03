//! Opening, unlocking and using a vault.

use std::{fmt, fs, io::Read as _, path::Path, time::Duration};

use inkveil_crypto::{
    KdfParams, Password, RecoveryKey, SecretKey, derive_password_key, fill_random, random_salt,
};
use rusqlite::{Connection, OptionalExtension, params};
use tracing::{info, warn};
use zeroize::Zeroizing;

use crate::{
    Attempts, NoteId, NoteListing, NoteMeta, NoteSummary, RevisionId, RevisionSummary, SlotId,
    SlotKind, StoreError, VaultHeader, VaultPaths, db,
    fsutil::atomic_write,
    header::MAX_HEADER_BYTES,
    instance_lock::InstanceLock,
    maintenance, now_ms,
    record::{RecordCipher, RecordKind, SaveId, new_save_id, save_id_from},
    throttle,
};

const HEADER_KEY: &str = "header";

/// `(id, sealed meta, sealed body, save id)` of a `notes` row.
type NoteRow = (Vec<u8>, Vec<u8>, Vec<u8>, Option<Vec<u8>>);
/// `(rev, note id, sealed meta, sealed body, save id)` of a `revisions` row.
type RevisionRow = (i64, Vec<u8>, Vec<u8>, Vec<u8>, Option<Vec<u8>>);
/// `(rev, note id, sealed meta, sealed body)` of a format-1 `revisions` row.
type LegacyRevisionRow = (i64, Vec<u8>, Vec<u8>, Vec<u8>);
/// Which record format the rows use. Set once every row is sealed with save-bound associated data.
const RECORDS_FORMAT_KEY: &str = "records_format";
const RECORDS_FORMAT: &[u8] = b"2";

/// A secret that can unlock a vault.
#[derive(Debug, Clone, Copy)]
pub enum Credential<'a> {
    /// The vault password.
    Password(&'a Password),
    /// The printed recovery key.
    RecoveryKey(&'a RecoveryKey),
    /// A wrapping key the operating system released for a platform slot.
    PlatformKey {
        /// The slot the key belongs to.
        slot: SlotId,
        /// The released wrapping key.
        key: &'a SecretKey,
    },
}

/// An open but still encrypted vault. Only the header can be read.
pub struct LockedVault {
    paths: VaultPaths,
    conn: Connection,
    header: VaultHeader,
    lock: InstanceLock,
}

/// An unlocked vault.
///
/// Dropping it (or calling [`Vault::lock`]) wipes the keys from memory.
pub struct Vault {
    paths: VaultPaths,
    pub(crate) conn: Connection,
    header: VaultHeader,
    lock: InstanceLock,
    master_key: SecretKey,
    pub(crate) records: RecordCipher,
}

/// A failed unlock attempt. Gives the locked vault back so the user can try again.
pub struct UnlockFailure {
    /// The vault, still locked.
    pub vault: LockedVault,
    /// Why unlocking failed.
    pub error: StoreError,
}

impl LockedVault {
    /// Opens the vault in `paths` without unlocking it.
    ///
    /// Takes the instance lock, checks and migrates the database structure, and reads the key
    /// header. If the header in the database cannot be read, the copy in `vault.header` is used.
    pub fn open(paths: VaultPaths) -> Result<Self, StoreError> {
        if !paths.exists() {
            return Err(StoreError::NotFound);
        }
        let lock = InstanceLock::acquire(&paths.lock())?;
        let mut conn = db::open(&paths.database(), false)?;
        db::check_and_migrate(&mut conn)?;
        let header = load_header(&conn, &paths)?;
        Ok(Self {
            paths,
            conn,
            header,
            lock,
        })
    }

    /// The vault's public header, e.g. to see which unlock methods are available.
    pub fn header(&self) -> &VaultHeader {
        &self.header
    }

    /// The vault's file locations.
    pub fn paths(&self) -> &VaultPaths {
        &self.paths
    }

    /// Wrong unlock attempts in a row so far.
    pub fn failed_attempts(&self) -> Attempts {
        throttle::load(&self.paths)
    }

    /// How long until the next unlock attempt is allowed, if attempts are paused.
    pub fn unlock_wait(&self) -> Option<Duration> {
        self.failed_attempts().wait(now_ms())
    }

    /// Unlocks the vault with `credential`.
    ///
    /// The first unlock after an upgrade re-seals every record in the current format, in one
    /// transaction.
    ///
    /// After [`FREE_ATTEMPTS`](crate::FREE_ATTEMPTS) wrong attempts in a row, further attempts are
    /// refused with [`StoreError::TooManyAttempts`] until a growing wait has passed (see
    /// [`wait_after`](crate::wait_after)). The count survives restarts; a correct unlock clears it.
    pub fn unlock(self, credential: Credential<'_>) -> Result<Vault, Box<UnlockFailure>> {
        if let Some(wait) = self.unlock_wait() {
            return Err(Box::new(UnlockFailure {
                vault: self,
                error: StoreError::TooManyAttempts(wait),
            }));
        }
        let vault_key = match self.unwrap_vault_key(credential) {
            Ok(key) => {
                throttle::clear(&self.paths);
                key
            }
            Err(error) => {
                if matches!(error, StoreError::WrongCredential) {
                    throttle::record_failure(&self.paths, now_ms());
                }
                return Err(Box::new(UnlockFailure { vault: self, error }));
            }
        };
        let mut vault = self.into_unlocked(vault_key);
        match vault.upgrade_records() {
            Ok(()) => Ok(vault),
            Err(error) => Err(Box::new(UnlockFailure {
                vault: vault.lock(),
                error,
            })),
        }
    }

    fn unwrap_vault_key(&self, credential: Credential<'_>) -> Result<SecretKey, StoreError> {
        match credential {
            Credential::Password(password) => {
                let slot = self
                    .header
                    .slots()
                    .iter()
                    .find(|s| matches!(s.kind, SlotKind::Password { .. }))
                    .ok_or(StoreError::NoSuchSlot)?;
                let SlotKind::Password { salt, kdf } = &slot.kind else {
                    unreachable!("filtered to password slots above");
                };
                let kek = derive_password_key(password, salt, kdf)?;
                self.header.unwrap(slot, &kek)
            }
            Credential::RecoveryKey(recovery) => {
                let slot = self
                    .header
                    .slots()
                    .iter()
                    .find(|s| matches!(s.kind, SlotKind::Recovery { .. }))
                    .ok_or(StoreError::NoSuchSlot)?;
                let SlotKind::Recovery { salt } = &slot.kind else {
                    unreachable!("filtered to recovery slots above");
                };
                self.header
                    .unwrap(slot, &recovery.derive_wrapping_key(salt))
            }
            Credential::PlatformKey { slot, key } => {
                let slot = self.header.slot(slot).ok_or(StoreError::NoSuchSlot)?;
                if !matches!(slot.kind, SlotKind::Platform { .. }) {
                    return Err(StoreError::NoSuchSlot);
                }
                self.header.unwrap(slot, key)
            }
        }
    }

    fn into_unlocked(self, vault_key: SecretKey) -> Vault {
        let records = RecordCipher::new(&vault_key, self.header.vault_id);
        info!("vault unlocked");
        Vault {
            paths: self.paths,
            conn: self.conn,
            header: self.header,
            lock: self.lock,
            master_key: vault_key,
            records,
        }
    }
}

impl Vault {
    /// Creates a new vault in `paths`, protected by `password`.
    ///
    /// The database is built under a temporary name and moved into place only once complete, so
    /// a crash during creation never leaves a half-made vault behind.
    pub fn create(
        paths: VaultPaths,
        password: &Password,
        kdf: KdfParams,
    ) -> Result<Self, StoreError> {
        password.check_policy()?;
        kdf.validate()?;
        fs::create_dir_all(paths.dir())?;
        let lock = InstanceLock::acquire(&paths.lock())?;
        if paths.exists() {
            return Err(StoreError::AlreadyExists);
        }
        if paths.has_vault_files() {
            return Err(StoreError::VaultFilesRemain);
        }

        let vault_key = SecretKey::generate()?;
        let mut vault_id = [0; 16];
        fill_random(&mut vault_id)?;
        let mut header = VaultHeader::new(vault_id, now_ms());
        let salt = random_salt()?;
        let kek = derive_password_key(password, &salt, &kdf)?;
        header.add_slot(SlotKind::Password { salt, kdf }, &kek, &vault_key)?;

        let staging = paths.database_in_progress();
        remove_if_exists(&staging)?;
        {
            let conn = db::open(&staging, true)?;
            db::create_schema(&conn)?;
            db::write_meta(&conn, HEADER_KEY, &header.encode()?)?;
            db::write_meta(&conn, RECORDS_FORMAT_KEY, RECORDS_FORMAT)?;
            // Closing checkpoints the WAL into the main file, leaving a single file to move.
            conn.close().map_err(|(_, e)| e)?;
        }
        fs::rename(&staging, paths.database())?;
        atomic_write(&paths.header(), &header.encode()?)?;

        let conn = db::open(&paths.database(), false)?;
        let records = RecordCipher::new(&vault_key, vault_id);
        info!("vault created");
        Ok(Self {
            paths,
            conn,
            header,
            lock,
            master_key: vault_key,
            records,
        })
    }

    /// Wipes the keys and returns the vault to the locked state.
    pub fn lock(self) -> LockedVault {
        info!("vault locked");
        LockedVault {
            paths: self.paths,
            conn: self.conn,
            header: self.header,
            lock: self.lock,
        }
        // `master_key` and `records` are dropped here, which zeroizes them.
    }

    /// The vault's public header.
    pub fn header(&self) -> &VaultHeader {
        &self.header
    }

    /// The vault's file locations.
    pub fn paths(&self) -> &VaultPaths {
        &self.paths
    }

    // ----- Notes -------------------------------------------------------------------------------

    /// Lists every note (including trashed ones) with its metadata. Bodies are not decrypted.
    ///
    /// A note that fails to decrypt is reported in [`NoteListing::unreadable`] instead of making
    /// the whole listing fail.
    pub fn notes(&self) -> Result<NoteListing, StoreError> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id, sealed_meta, save_id FROM notes")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Option<Vec<u8>>>(2)?,
            ))
        })?;
        let mut listing = NoteListing::default();
        for row in rows {
            let (id, sealed, save) = row?;
            let id = NoteId::from_slice(&id)?;
            let meta = save_id_from(save)
                .ok_or(StoreError::Corrupt("note has no save id"))
                .and_then(|save| {
                    let plain = self.records.open(
                        RecordKind::NoteMeta {
                            id: &id,
                            save: &save,
                        },
                        &sealed,
                    )?;
                    NoteMeta::decode(&plain)
                });
            match meta {
                Ok(meta) => listing.notes.push(NoteSummary { id, meta }),
                Err(error) => {
                    warn!("note {id} could not be read: {error}");
                    listing.unreadable.push(id);
                }
            }
        }
        Ok(listing)
    }

    /// Creates a note and returns its id.
    pub fn create_note(&self, meta: &NoteMeta, body: &[u8]) -> Result<NoteId, StoreError> {
        let id = NoteId::generate()?;
        self.save_note(&id, meta, body)?;
        Ok(id)
    }

    /// Saves a note's metadata and body in one statement, creating the note if needed.
    pub fn save_note(&self, id: &NoteId, meta: &NoteMeta, body: &[u8]) -> Result<(), StoreError> {
        let save = new_save_id()?;
        let sealed_meta = self
            .records
            .seal(RecordKind::NoteMeta { id, save: &save }, &meta.encode()?)?;
        let sealed_body = self
            .records
            .seal(RecordKind::NoteBody { id, save: &save }, body)?;
        self.conn
            .prepare_cached(
                "INSERT INTO notes (id, sealed_meta, sealed_body, save_id) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (id) DO UPDATE SET
                     sealed_meta = excluded.sealed_meta,
                     sealed_body = excluded.sealed_body,
                     save_id = excluded.save_id",
            )?
            .execute(params![
                id.as_bytes(),
                sealed_meta,
                sealed_body,
                save.as_slice()
            ])?;
        Ok(())
    }

    /// Saves only a note's metadata (rename, tags, pin, trash), bound to its current body.
    pub fn save_meta(&self, id: &NoteId, meta: &NoteMeta) -> Result<(), StoreError> {
        let save = self.note_save_id(id)?;
        let sealed_meta = self
            .records
            .seal(RecordKind::NoteMeta { id, save: &save }, &meta.encode()?)?;
        self.conn
            .prepare_cached("UPDATE notes SET sealed_meta = ?2 WHERE id = ?1")?
            .execute(params![id.as_bytes(), sealed_meta])?;
        Ok(())
    }

    fn note_save_id(&self, id: &NoteId) -> Result<SaveId, StoreError> {
        let save: Option<Vec<u8>> = self
            .conn
            .prepare_cached("SELECT save_id FROM notes WHERE id = ?1")?
            .query_row([id.as_bytes()], |r| r.get(0))
            .optional()?
            .ok_or(StoreError::NoteNotFound(*id))?;
        save_id_from(save).ok_or(StoreError::Corrupt("note has no save id"))
    }

    /// Decrypts a note's metadata.
    pub fn load_meta(&self, id: &NoteId) -> Result<NoteMeta, StoreError> {
        let (sealed, save) = self.note_column(id, "sealed_meta")?;
        NoteMeta::decode(
            &self
                .records
                .open(RecordKind::NoteMeta { id, save: &save }, &sealed)?,
        )
    }

    /// Decrypts a note's body. The returned buffer is wiped when dropped.
    pub fn load_body(&self, id: &NoteId) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        let (sealed, save) = self.note_column(id, "sealed_body")?;
        self.records
            .open(RecordKind::NoteBody { id, save: &save }, &sealed)
    }

    fn note_column(&self, id: &NoteId, column: &str) -> Result<(Vec<u8>, SaveId), StoreError> {
        let sql = match column {
            "sealed_meta" => "SELECT sealed_meta, save_id FROM notes WHERE id = ?1",
            _ => "SELECT sealed_body, save_id FROM notes WHERE id = ?1",
        };
        let (sealed, save): (Vec<u8>, Option<Vec<u8>>) = self
            .conn
            .prepare_cached(sql)?
            .query_row([id.as_bytes()], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
            .ok_or(StoreError::NoteNotFound(*id))?;
        Ok((
            sealed,
            save_id_from(save).ok_or(StoreError::Corrupt("note has no save id"))?,
        ))
    }

    /// Permanently deletes a note and all its revisions. Prefer moving notes to the trash.
    pub fn delete_note_permanently(&mut self, id: &NoteId) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM revisions WHERE note_id = ?1", [id.as_bytes()])?;
        tx.execute("DELETE FROM notes WHERE id = ?1", [id.as_bytes()])?;
        tx.commit()?;
        Ok(())
    }

    // ----- Revisions ---------------------------------------------------------------------------

    /// Stores the note's current saved state as a revision.
    ///
    /// Revisions are re-sealed under their own associated data and save id, so a revision can
    /// never be copied back over the live note to silently undo an edit.
    pub fn snapshot_revision(&self, id: &NoteId) -> Result<RevisionId, StoreError> {
        let meta = self.load_meta(id)?;
        let body = self.load_body(id)?;
        let save = new_save_id()?;
        let sealed_meta = self.records.seal(
            RecordKind::RevisionMeta { id, save: &save },
            &meta.encode()?,
        )?;
        let sealed_body = self
            .records
            .seal(RecordKind::RevisionBody { id, save: &save }, &body)?;
        self.conn.execute(
            "INSERT INTO revisions (note_id, sealed_meta, sealed_body, save_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![id.as_bytes(), sealed_meta, sealed_body, save.as_slice()],
        )?;
        Ok(RevisionId(self.conn.last_insert_rowid()))
    }

    /// Lists a note's readable revisions, oldest first.
    pub fn revisions(&self, id: &NoteId) -> Result<Vec<RevisionSummary>, StoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT rev, sealed_meta, save_id FROM revisions WHERE note_id = ?1 ORDER BY rev",
        )?;
        let rows = stmt.query_map([id.as_bytes()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Option<Vec<u8>>>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (rev, sealed, save) = row?;
            let meta = save_id_from(save)
                .ok_or(StoreError::Corrupt("revision has no save id"))
                .and_then(|save| {
                    let plain = self
                        .records
                        .open(RecordKind::RevisionMeta { id, save: &save }, &sealed)?;
                    NoteMeta::decode(&plain)
                });
            match meta {
                Ok(meta) => out.push(RevisionSummary {
                    id: RevisionId(rev),
                    meta,
                }),
                Err(error) => warn!("revision {rev} of note {id} could not be read: {error}"),
            }
        }
        Ok(out)
    }

    /// Decrypts the body of one revision of note `id`.
    pub fn load_revision_body(
        &self,
        id: &NoteId,
        revision: RevisionId,
    ) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        let (sealed, save): (Vec<u8>, Option<Vec<u8>>) = self
            .conn
            .prepare_cached(
                "SELECT sealed_body, save_id FROM revisions WHERE rev = ?1 AND note_id = ?2",
            )?
            .query_row(params![revision.0, id.as_bytes()], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?
            .ok_or(StoreError::NoteNotFound(*id))?;
        let save = save_id_from(save).ok_or(StoreError::Corrupt("revision has no save id"))?;
        self.records
            .open(RecordKind::RevisionBody { id, save: &save }, &sealed)
    }

    /// Deletes revisions of note `id` for which `discard` returns `true`.
    pub fn prune_revisions(
        &mut self,
        id: &NoteId,
        mut discard: impl FnMut(&RevisionSummary) -> bool,
    ) -> Result<usize, StoreError> {
        let doomed: Vec<i64> = self
            .revisions(id)?
            .iter()
            .filter(|r| discard(r))
            .map(|r| r.id.0)
            .collect();
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached("DELETE FROM revisions WHERE rev = ?1")?;
            for rev in &doomed {
                stmt.execute([rev])?;
            }
        }
        tx.commit()?;
        Ok(doomed.len())
    }

    /// Thins a note's history with [`maintenance::revisions_to_keep`]: recent revisions are all
    /// kept, older ones one per day for a month, and the very first one is always kept.
    pub fn thin_revisions(&mut self, id: &NoteId, now: i64) -> Result<usize, StoreError> {
        let revisions = self.revisions(id)?;
        let keep = maintenance::revisions_to_keep(
            &revisions
                .iter()
                .map(|r| (r.id, r.meta.modified_ms))
                .collect::<Vec<_>>(),
            now,
        );
        self.prune_revisions(id, |r| !keep.contains(&r.id))
    }

    // ----- App state ---------------------------------------------------------------------------

    /// Stores an encrypted piece of app state (open tabs, personal dictionary, settings).
    pub fn put_state(&self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        let sealed = self.records.seal(RecordKind::State(key), value)?;
        self.conn
            .prepare_cached(
                "INSERT INTO state (key, sealed) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET sealed = excluded.sealed",
            )?
            .execute(params![key, sealed])?;
        Ok(())
    }

    /// Reads a piece of app state stored with [`Vault::put_state`].
    pub fn get_state(&self, key: &str) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
        let sealed: Option<Vec<u8>> = self
            .conn
            .prepare_cached("SELECT sealed FROM state WHERE key = ?1")?
            .query_row([key], |r| r.get(0))
            .optional()?;
        sealed
            .map(|s| self.records.open(RecordKind::State(key), &s))
            .transpose()
    }

    // ----- Unlock methods ----------------------------------------------------------------------

    /// Replaces the password **and the vault key**.
    ///
    /// Every record is re-encrypted under a new key in one transaction, so an old copy of the
    /// header or database together with the old password cannot read anything written from now
    /// on. Platform unlock slots are removed (they wrap the old key). If the vault had a recovery
    /// key, a new one replaces it and is returned for the user to write down. Old backups are
    /// deleted, because they open with the old password, and a fresh backup is made.
    pub fn change_password(
        &mut self,
        password: &Password,
        kdf: KdfParams,
    ) -> Result<Option<RecoveryKey>, StoreError> {
        password.check_policy()?;
        kdf.validate()?;
        let new_key = SecretKey::generate()?;
        let new_records = RecordCipher::new(&new_key, self.header.vault_id);

        let mut header = VaultHeader::new(self.header.vault_id, self.header.created_ms);
        let salt = random_salt()?;
        let kek = derive_password_key(password, &salt, &kdf)?;
        header.add_slot(SlotKind::Password { salt, kdf }, &kek, &new_key)?;
        let recovery = if self.header.has_recovery_key() {
            let recovery = RecoveryKey::generate()?;
            let salt = random_salt()?;
            header.add_slot(
                SlotKind::Recovery { salt },
                &recovery.derive_wrapping_key(&salt),
                &new_key,
            )?;
            Some(recovery)
        } else {
            None
        };

        let encoded = header.encode()?;
        let tx = self.conn.transaction()?;
        reseal_all(&tx, &self.records, &new_records)?;
        db::write_meta(&tx, HEADER_KEY, &encoded)?;
        tx.commit()?;
        atomic_write(&self.paths.header(), &encoded)?;

        self.header = header;
        self.master_key = new_key;
        self.records = new_records;
        info!("vault key rotated");

        maintenance::delete_backups(&self.paths)?;
        self.backup_now()?;
        Ok(recovery)
    }

    /// Creates a new recovery key, replacing any previous one, and returns it for the user to
    /// write down. It is not stored anywhere in readable form.
    pub fn create_recovery_key(&mut self) -> Result<RecoveryKey, StoreError> {
        let recovery = RecoveryKey::generate()?;
        let salt = random_salt()?;
        let mut header = self.header.clone();
        header.remove_slots(|s| matches!(s.kind, SlotKind::Recovery { .. }));
        header.add_slot(
            SlotKind::Recovery { salt },
            &recovery.derive_wrapping_key(&salt),
            &self.master_key,
        )?;
        self.write_header(header)?;
        Ok(recovery)
    }

    /// Adds a slot for a platform unlock method, replacing earlier slots of the same provider.
    pub fn add_platform_slot(
        &mut self,
        provider: &str,
        data: Vec<u8>,
        wrapping_key: &SecretKey,
    ) -> Result<SlotId, StoreError> {
        let mut header = self.header.clone();
        header.remove_slots(
            |s| matches!(&s.kind, SlotKind::Platform { provider: p, .. } if p == provider),
        );
        let id = header.add_slot(
            SlotKind::Platform {
                provider: provider.to_owned(),
                data,
            },
            wrapping_key,
            &self.master_key,
        )?;
        self.write_header(header)?;
        Ok(id)
    }

    /// Removes a slot. The password slot cannot be removed, so the vault always has one.
    pub fn remove_slot(&mut self, id: SlotId) -> Result<(), StoreError> {
        let slot = self.header.slot(id).ok_or(StoreError::NoSuchSlot)?;
        if matches!(slot.kind, SlotKind::Password { .. }) {
            return Err(StoreError::LastPasswordSlot);
        }
        let mut header = self.header.clone();
        header.remove_slots(|s| s.id == id);
        self.write_header(header)
    }

    /// Commits a new header to the database first (the source of truth), then to the copy.
    ///
    /// If the process dies between the two writes, the copy still holds the previous slots, which
    /// wrap the same vault key, so it remains a valid fallback.
    fn write_header(&mut self, header: VaultHeader) -> Result<(), StoreError> {
        let encoded = header.encode()?;
        db::write_meta(&self.conn, HEADER_KEY, &encoded)?;
        atomic_write(&self.paths.header(), &encoded)?;
        self.header = header;
        Ok(())
    }

    // ----- Maintenance -------------------------------------------------------------------------

    /// Runs SQLite's structural check. Returns `true` when the database is healthy.
    pub fn quick_check(&self) -> Result<bool, StoreError> {
        db::quick_check(&self.conn)
    }

    /// Writes a consistent copy of the whole database to `dest`, which must not exist yet.
    ///
    /// The copy is as encrypted as the vault itself and includes the key header, so it opens with
    /// the same password.
    pub fn backup_to(&self, dest: &Path) -> Result<(), StoreError> {
        let dest = dest
            .to_str()
            .ok_or(StoreError::Corrupt("backup path is not valid Unicode"))?;
        self.conn.execute("VACUUM INTO ?1", [dest])?;
        Ok(())
    }

    /// Makes a rotating backup if the newest one is older than `max_age_ms`, then deletes backups
    /// no longer needed (see [`maintenance::backups_to_delete`]). Returns the new backup's path.
    pub fn backup_if_due(&self, max_age_ms: i64) -> Result<Option<std::path::PathBuf>, StoreError> {
        let newest = maintenance::list_backups(&self.paths)?
            .first()
            .map(|(_, at)| *at);
        if newest.is_some_and(|at| now_ms().saturating_sub(at) < max_age_ms) {
            return Ok(None);
        }
        self.backup_now().map(Some)
    }

    fn backup_now(&self) -> Result<std::path::PathBuf, StoreError> {
        maintenance::write_backup(&self.paths, |tmp| self.backup_to(tmp))
    }

    /// Re-seals rows written by an older record format. Runs once, in one transaction.
    fn upgrade_records(&mut self) -> Result<(), StoreError> {
        if db::read_meta(&self.conn, RECORDS_FORMAT_KEY)?.as_deref() == Some(RECORDS_FORMAT) {
            return Ok(());
        }
        info!("upgrading record format");
        let tx = self.conn.transaction()?;
        upgrade_legacy_rows(&tx, &self.records)?;
        db::write_meta(&tx, RECORDS_FORMAT_KEY, RECORDS_FORMAT)?;
        tx.commit()?;
        Ok(())
    }
}

/// Re-seals every format-1 row (no save id) of `notes` and `revisions` in the current format.
fn upgrade_legacy_rows(conn: &Connection, records: &RecordCipher) -> Result<(), StoreError> {
    let notes: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> = conn
        .prepare("SELECT id, sealed_meta, sealed_body FROM notes WHERE save_id IS NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    for (id, meta, body) in notes {
        let id = NoteId::from_slice(&id)?;
        let meta = records.open(RecordKind::LegacyNoteMeta(&id), &meta)?;
        let body = records.open(RecordKind::LegacyNoteBody(&id), &body)?;
        let save = new_save_id()?;
        conn.execute(
            "UPDATE notes SET sealed_meta = ?2, sealed_body = ?3, save_id = ?4 WHERE id = ?1",
            params![
                id.as_bytes(),
                records.seal(
                    RecordKind::NoteMeta {
                        id: &id,
                        save: &save
                    },
                    &meta
                )?,
                records.seal(
                    RecordKind::NoteBody {
                        id: &id,
                        save: &save
                    },
                    &body
                )?,
                save.as_slice()
            ],
        )?;
    }
    let revisions: Vec<LegacyRevisionRow> = conn
        .prepare(
            "SELECT rev, note_id, sealed_meta, sealed_body FROM revisions WHERE save_id IS NULL",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    for (rev, id, meta, body) in revisions {
        let id = NoteId::from_slice(&id)?;
        // Format-1 revisions were byte-for-byte copies of the note's sealed fields.
        let meta = records.open(RecordKind::LegacyNoteMeta(&id), &meta)?;
        let body = records.open(RecordKind::LegacyNoteBody(&id), &body)?;
        let save = new_save_id()?;
        conn.execute(
            "UPDATE revisions SET sealed_meta = ?2, sealed_body = ?3, save_id = ?4 WHERE rev = ?1",
            params![
                rev,
                records.seal(
                    RecordKind::RevisionMeta {
                        id: &id,
                        save: &save
                    },
                    &meta
                )?,
                records.seal(
                    RecordKind::RevisionBody {
                        id: &id,
                        save: &save
                    },
                    &body
                )?,
                save.as_slice()
            ],
        )?;
    }
    Ok(())
}

/// Decrypts every record with `from` and re-encrypts it with `to`. Fails without changes if any
/// record cannot be read, so nothing is left behind under a key that no longer exists.
fn reseal_all(conn: &Connection, from: &RecordCipher, to: &RecordCipher) -> Result<(), StoreError> {
    let notes: Vec<NoteRow> = conn
        .prepare("SELECT id, sealed_meta, sealed_body, save_id FROM notes")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    for (id, meta, body, save) in notes {
        let id = NoteId::from_slice(&id)?;
        let save = save_id_from(save).ok_or(StoreError::Corrupt("note has no save id"))?;
        let meta = from.open(
            RecordKind::NoteMeta {
                id: &id,
                save: &save,
            },
            &meta,
        )?;
        let body = from.open(
            RecordKind::NoteBody {
                id: &id,
                save: &save,
            },
            &body,
        )?;
        conn.execute(
            "UPDATE notes SET sealed_meta = ?2, sealed_body = ?3 WHERE id = ?1",
            params![
                id.as_bytes(),
                to.seal(
                    RecordKind::NoteMeta {
                        id: &id,
                        save: &save
                    },
                    &meta
                )?,
                to.seal(
                    RecordKind::NoteBody {
                        id: &id,
                        save: &save
                    },
                    &body
                )?
            ],
        )?;
    }
    let revisions: Vec<RevisionRow> = conn
        .prepare("SELECT rev, note_id, sealed_meta, sealed_body, save_id FROM revisions")?
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<_, _>>()?;
    for (rev, id, meta, body, save) in revisions {
        let id = NoteId::from_slice(&id)?;
        let save = save_id_from(save).ok_or(StoreError::Corrupt("revision has no save id"))?;
        let meta = from.open(
            RecordKind::RevisionMeta {
                id: &id,
                save: &save,
            },
            &meta,
        )?;
        let body = from.open(
            RecordKind::RevisionBody {
                id: &id,
                save: &save,
            },
            &body,
        )?;
        conn.execute(
            "UPDATE revisions SET sealed_meta = ?2, sealed_body = ?3 WHERE rev = ?1",
            params![
                rev,
                to.seal(
                    RecordKind::RevisionMeta {
                        id: &id,
                        save: &save
                    },
                    &meta
                )?,
                to.seal(
                    RecordKind::RevisionBody {
                        id: &id,
                        save: &save
                    },
                    &body
                )?
            ],
        )?;
    }
    let state: Vec<(String, Vec<u8>)> = conn
        .prepare("SELECT key, sealed FROM state")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (key, sealed) in state {
        let plain = from.open(RecordKind::State(&key), &sealed)?;
        conn.execute(
            "UPDATE state SET sealed = ?2 WHERE key = ?1",
            params![key, to.seal(RecordKind::State(&key), &plain)?],
        )?;
    }
    crate::blobs::reseal_blobs(conn, from, to)
}

impl fmt::Debug for LockedVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LockedVault")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for UnlockFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnlockFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for UnlockFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for UnlockFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Reads the header from the database, falling back to (and repairing from) the copy.
///
/// Any database error reading the header (missing row, damaged page, undecodable bytes) falls
/// through to the copy.
fn load_header(conn: &Connection, paths: &VaultPaths) -> Result<VaultHeader, StoreError> {
    let from_db = db::read_meta(conn, HEADER_KEY)
        .and_then(|row| row.ok_or(StoreError::Corrupt("header missing")))
        .and_then(|bytes| VaultHeader::decode(&bytes));
    let copy = read_header_copy(paths);

    match from_db {
        Ok(header) => {
            let encoded = header.encode()?;
            if copy.as_deref().ok() != Some(encoded.as_slice()) {
                warn!("header copy missing or stale; rewriting it");
                atomic_write(&paths.header(), &encoded)?;
            }
            Ok(header)
        }
        Err(db_error) => {
            warn!("header in database unreadable ({db_error}); using the copy");
            let header = VaultHeader::decode(&copy?)?;
            if let Err(error) = db::write_meta(conn, HEADER_KEY, &header.encode()?) {
                warn!("could not repair the header in the database: {error}");
            }
            Ok(header)
        }
    }
}

/// Reads `vault.header`, refusing files larger than any real header.
fn read_header_copy(paths: &VaultPaths) -> Result<Vec<u8>, StoreError> {
    let mut bytes = Vec::new();
    fs::File::open(paths.header())?
        .take(MAX_HEADER_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_HEADER_BYTES {
        return Err(StoreError::Corrupt("header copy too large"));
    }
    Ok(bytes)
}

fn remove_if_exists(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "upgrade test 1";

    #[test]
    fn format_1_rows_are_upgraded_on_unlock() {
        let dir = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(dir.path());
        let vault =
            Vault::create(paths.clone(), &Password::new(PASSWORD), KdfParams::TESTING).unwrap();

        // Write a note and a revision exactly as format 1 did, and mark the vault as format 1.
        let id = NoteId::generate().unwrap();
        let meta = NoteMeta::new("legacy").encode().unwrap();
        let sealed_meta = vault
            .records
            .seal(RecordKind::LegacyNoteMeta(&id), &meta)
            .unwrap();
        let sealed_body = vault
            .records
            .seal(RecordKind::LegacyNoteBody(&id), b"legacy body")
            .unwrap();
        vault
            .conn
            .execute(
                "INSERT INTO notes (id, sealed_meta, sealed_body) VALUES (?1, ?2, ?3)",
                params![id.as_bytes(), sealed_meta, sealed_body],
            )
            .unwrap();
        vault
            .conn
            .execute(
                "INSERT INTO revisions (note_id, sealed_meta, sealed_body) VALUES (?1, ?2, ?3)",
                params![id.as_bytes(), sealed_meta, sealed_body],
            )
            .unwrap();
        vault
            .conn
            .execute("DELETE FROM meta WHERE key = ?1", [RECORDS_FORMAT_KEY])
            .unwrap();
        drop(vault);

        let vault = LockedVault::open(paths.clone())
            .unwrap()
            .unlock(Credential::Password(&Password::new(PASSWORD)))
            .unwrap();
        assert_eq!(vault.notes().unwrap().notes[0].meta.title, "legacy");
        assert_eq!(vault.load_body(&id).unwrap().as_slice(), b"legacy body");
        let revisions = vault.revisions(&id).unwrap();
        assert_eq!(revisions.len(), 1);
        assert_eq!(
            vault
                .load_revision_body(&id, revisions[0].id)
                .unwrap()
                .as_slice(),
            b"legacy body"
        );
        // No rows are left in the old format.
        let legacy: i64 = vault
            .conn
            .query_row(
                "SELECT COUNT(*) FROM notes WHERE save_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(legacy, 0);
    }
}
