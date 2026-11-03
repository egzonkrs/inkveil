//! End-to-end tests of the vault through its public API.

#![allow(clippy::unwrap_used)]

use std::fs;

use inkveil_crypto::{KdfParams, Password, SecretKey};
use inkveil_store::{
    Credential, FREE_ATTEMPTS, LockedVault, NoteMeta, SlotKind, StoreError, Vault, VaultPaths,
    maintenance,
};
use rusqlite::Connection;

const PASSWORD: &str = "correct horse 7";

fn new_vault() -> (tempfile::TempDir, Vault) {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::create(
        VaultPaths::new(dir.path().join("vault")),
        &Password::new(PASSWORD),
        KdfParams::TESTING,
    )
    .unwrap();
    (dir, vault)
}

fn reopen(vault: Vault) -> LockedVault {
    let paths = vault.paths().clone();
    drop(vault);
    LockedVault::open(paths).unwrap()
}

fn unlock(locked: LockedVault) -> Vault {
    locked
        .unlock(Credential::Password(&Password::new(PASSWORD)))
        .unwrap()
}

#[test]
fn notes_survive_reopen() {
    let (_dir, vault) = new_vault();
    let meta = NoteMeta {
        tags: vec!["work".into()],
        ..NoteMeta::new("Plans")
    };
    let id = vault.create_note(&meta, b"secret body").unwrap();

    let vault = unlock(reopen(vault));
    let notes = vault.notes().unwrap().notes;
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].id, id);
    assert_eq!(notes[0].meta, meta);
    assert_eq!(vault.load_body(&id).unwrap().as_slice(), b"secret body");
}

#[test]
fn nothing_readable_on_disk() {
    let (_dir, vault) = new_vault();
    vault
        .create_note(&NoteMeta::new("TopSecretTitle"), b"TopSecretBody")
        .unwrap();
    vault.put_state("session", b"TopSecretState").unwrap();
    let paths = vault.paths().clone();
    drop(vault);

    for entry in fs::read_dir(paths.dir()).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("TopSecret"), "plaintext found on disk");
    }
}

#[test]
fn wrong_password_keeps_vault_locked() {
    let (_dir, vault) = new_vault();
    let locked = reopen(vault);
    let failure = locked
        .unlock(Credential::Password(&Password::new("wrong password 1")))
        .unwrap_err();
    assert!(matches!(failure.error, StoreError::WrongCredential));
    // The vault comes back so the user can try again.
    unlock(failure.vault);
}

#[test]
fn repeated_wrong_passwords_pause_unlocking_even_after_reopening() {
    let (_dir, vault) = new_vault();
    let mut locked = reopen(vault);
    for _ in 0..=FREE_ATTEMPTS {
        let failure = locked
            .unlock(Credential::Password(&Password::new("wrong password 1")))
            .unwrap_err();
        assert!(matches!(failure.error, StoreError::WrongCredential));
        locked = failure.vault;
    }
    // Even the right password is refused during the wait, and reopening does not reset it.
    let paths = locked.paths().clone();
    drop(locked);
    let locked = LockedVault::open(paths).unwrap();
    assert!(locked.unlock_wait().is_some());
    let failure = locked
        .unlock(Credential::Password(&Password::new(PASSWORD)))
        .unwrap_err();
    assert!(matches!(failure.error, StoreError::TooManyAttempts(_)));
    assert_eq!(failure.vault.failed_attempts().failures, FREE_ATTEMPTS + 1);
}

#[test]
fn a_correct_unlock_resets_the_count() {
    let (_dir, vault) = new_vault();
    let locked = reopen(vault);
    let failure = locked
        .unlock(Credential::Password(&Password::new("wrong password 1")))
        .unwrap_err();
    assert_eq!(failure.vault.failed_attempts().failures, 1);
    let vault = unlock(failure.vault);
    assert_eq!(reopen(vault).failed_attempts().failures, 0);
}

#[test]
fn blobs_are_encrypted_deduplicated_and_survive_a_password_change() {
    let (_dir, mut vault) = new_vault();
    let picture = b"PNG-ish TopSecretPicture bytes".repeat(50);
    let id = vault.put_blob(&picture).unwrap();
    assert_eq!(
        vault.put_blob(&picture).unwrap(),
        id,
        "same bytes are stored once"
    );
    assert_eq!(vault.load_blob(id).unwrap().as_slice(), picture.as_slice());

    vault
        .change_password(&Password::new("another pass 9"), KdfParams::TESTING)
        .unwrap();
    assert_eq!(vault.load_blob(id).unwrap().as_slice(), picture.as_slice());
    assert_eq!(
        vault.put_blob(&picture).unwrap(),
        id,
        "fingerprints follow the new key"
    );

    let paths = vault.paths().clone();
    drop(vault);
    for entry in fs::read_dir(paths.dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = fs::read(path).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("TopSecretPicture"));
        }
    }
}

#[test]
fn unused_blobs_are_removed_only_when_old_enough() {
    let (_dir, vault) = new_vault();
    let kept = vault.put_blob(b"kept").unwrap();
    let unused = vault.put_blob(b"unused").unwrap();
    let in_use = std::collections::HashSet::from([kept]);
    assert_eq!(vault.delete_unused_blobs(&in_use, 60_000).unwrap(), 0);
    assert_eq!(vault.delete_unused_blobs(&in_use, -1).unwrap(), 1);
    assert!(vault.load_blob(unused).is_err());
    assert!(vault.load_blob(kept).is_ok());
}

#[test]
fn second_instance_is_refused() {
    let (_dir, vault) = new_vault();
    let err = LockedVault::open(vault.paths().clone()).unwrap_err();
    assert!(matches!(err, StoreError::AlreadyOpen));
}

#[test]
fn creating_over_an_existing_vault_fails() {
    let (_dir, vault) = new_vault();
    let paths = vault.paths().clone();
    drop(vault);
    let err = Vault::create(paths, &Password::new(PASSWORD), KdfParams::TESTING).unwrap_err();
    assert!(matches!(err, StoreError::AlreadyExists));
}

#[test]
fn creating_where_a_vault_lost_its_database_fails() {
    let (_dir, vault) = new_vault();
    vault.backup_if_due(0).unwrap();
    let paths = vault.paths().clone();
    drop(vault);
    fs::remove_file(paths.database()).unwrap();
    assert!(!paths.exists() && paths.has_vault_files());
    let err = Vault::create(paths.clone(), &Password::new(PASSWORD), KdfParams::TESTING);
    assert!(matches!(err, Err(StoreError::VaultFilesRemain)));

    // Backups alone are enough to refuse, and nothing of the old vault is touched.
    fs::remove_file(paths.header()).unwrap();
    let err = Vault::create(paths.clone(), &Password::new(PASSWORD), KdfParams::TESTING);
    assert!(matches!(err, Err(StoreError::VaultFilesRemain)));
    assert_eq!(
        inkveil_store::maintenance::list_backups(&paths)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn weak_password_is_refused_at_creation() {
    let dir = tempfile::tempdir().unwrap();
    let err = Vault::create(
        VaultPaths::new(dir.path()),
        &Password::new("short1"),
        KdfParams::TESTING,
    )
    .unwrap_err();
    assert!(matches!(err, StoreError::PasswordPolicy(_)));
}

#[test]
fn recovery_key_unlocks() {
    let (_dir, mut vault) = new_vault();
    let recovery = vault.create_recovery_key().unwrap();
    assert!(vault.header().has_recovery_key());
    let shown = recovery.display();

    let typed = inkveil_crypto::RecoveryKey::parse(&shown.to_lowercase()).unwrap();
    let vault = reopen(vault)
        .unlock(Credential::RecoveryKey(&typed))
        .unwrap();
    assert_eq!(vault.notes().unwrap().notes.len(), 0);
}

#[test]
fn revision_cannot_be_copied_over_the_note() {
    let (_dir, vault) = new_vault();
    let id = vault.create_note(&NoteMeta::new("n"), b"old text").unwrap();
    vault.snapshot_revision(&id).unwrap();
    vault
        .save_note(&id, &NoteMeta::new("n"), b"new text")
        .unwrap();
    let paths = vault.paths().clone();
    drop(vault);

    // An attacker copies the revision's sealed fields (and save id) into the live note row.
    let conn = Connection::open(paths.database()).unwrap();
    conn.execute(
        "UPDATE notes SET sealed_meta = r.sealed_meta, sealed_body = r.sealed_body,
                          save_id = r.save_id
         FROM (SELECT sealed_meta, sealed_body, save_id FROM revisions) AS r",
        [],
    )
    .unwrap();
    drop(conn);

    let vault = unlock(LockedVault::open(paths).unwrap());
    assert!(vault.load_body(&id).is_err());
    let listing = vault.notes().unwrap();
    assert_eq!(listing.unreadable, [id]);
}

#[test]
fn halves_of_different_saves_cannot_be_mixed() {
    let (_dir, vault) = new_vault();
    let id = vault
        .create_note(&NoteMeta::new("first"), b"first body")
        .unwrap();
    let paths = vault.paths().clone();
    let conn = Connection::open(paths.database()).unwrap();
    let old_meta: Vec<u8> = conn
        .query_row("SELECT sealed_meta FROM notes", [], |r| r.get(0))
        .unwrap();
    vault
        .save_note(&id, &NoteMeta::new("second"), b"second body")
        .unwrap();
    drop(vault);

    conn.execute("UPDATE notes SET sealed_meta = ?1", [old_meta])
        .unwrap();
    drop(conn);
    let vault = unlock(LockedVault::open(paths).unwrap());
    assert!(vault.load_meta(&id).is_err());
    // The body is still intact and readable on its own.
    assert_eq!(vault.load_body(&id).unwrap().as_slice(), b"second body");
}

#[test]
fn one_damaged_note_does_not_hide_the_others() {
    let (_dir, vault) = new_vault();
    let good = vault.create_note(&NoteMeta::new("good"), b"fine").unwrap();
    let bad = vault.create_note(&NoteMeta::new("bad"), b"doomed").unwrap();
    let paths = vault.paths().clone();
    drop(vault);
    let conn = Connection::open(paths.database()).unwrap();
    conn.execute(
        "UPDATE notes SET sealed_meta = x'01020304' WHERE id = ?1",
        [bad.as_bytes().as_slice()],
    )
    .unwrap();
    drop(conn);
    let vault = unlock(LockedVault::open(paths).unwrap());
    let listing = vault.notes().unwrap();
    assert_eq!(listing.notes.len(), 1);
    assert_eq!(listing.notes[0].id, good);
    assert_eq!(listing.unreadable, [bad]);
}

#[test]
fn password_change_rotates_the_vault_key() {
    let (dir, mut vault) = new_vault();
    vault.create_recovery_key().unwrap();
    let id = vault.create_note(&NoteMeta::new("n"), b"before").unwrap();
    let paths = vault.paths().clone();
    // An attacker kept a copy of the old header and database.
    let stolen = dir.path().join("stolen");
    fs::create_dir_all(&stolen).unwrap();
    let stolen_paths = VaultPaths::new(&stolen);
    vault.backup_to(&stolen_paths.database()).unwrap();

    let new_recovery = vault
        .change_password(&Password::new("new password 2"), KdfParams::TESTING)
        .unwrap();
    assert!(
        new_recovery.is_some(),
        "a new recovery key replaces the old one"
    );
    vault.save_note(&id, &NoteMeta::new("n"), b"after").unwrap();
    assert_eq!(vault.header().slots().len(), 2);
    drop(vault);

    // The old password opens the stolen copy, but its key cannot read anything new: plant the
    // new database rows into the stolen copy and try.
    let stolen_vault = unlock(LockedVault::open(stolen_paths.clone()).unwrap());
    drop(stolen_vault);
    let new_body: Vec<u8> = Connection::open(paths.database())
        .unwrap()
        .query_row("SELECT sealed_body FROM notes", [], |r| r.get(0))
        .unwrap();
    let new_save: Vec<u8> = Connection::open(paths.database())
        .unwrap()
        .query_row("SELECT save_id FROM notes", [], |r| r.get(0))
        .unwrap();
    Connection::open(stolen_paths.database())
        .unwrap()
        .execute(
            "UPDATE notes SET sealed_body = ?1, save_id = ?2",
            rusqlite::params![new_body, new_save],
        )
        .unwrap();
    let stolen_vault = unlock(LockedVault::open(stolen_paths).unwrap());
    assert!(stolen_vault.load_body(&id).is_err());

    // The real vault opens with the new password and has the new text.
    let failure = LockedVault::open(paths.clone())
        .unwrap()
        .unlock(Credential::Password(&Password::new(PASSWORD)))
        .unwrap_err();
    assert!(matches!(failure.error, StoreError::WrongCredential));
    let vault = failure
        .vault
        .unlock(Credential::Password(&Password::new("new password 2")))
        .unwrap();
    assert_eq!(vault.load_body(&id).unwrap().as_slice(), b"after");
    // Backups were replaced by one made with the new key.
    assert_eq!(
        inkveil_store::maintenance::list_backups(&paths)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn rotating_backups() {
    let (_dir, vault) = new_vault();
    vault.create_note(&NoteMeta::new("n"), b"x").unwrap();
    let first = vault.backup_if_due(24 * 60 * 60 * 1000).unwrap();
    assert!(first.is_some());
    // Not due again right away.
    assert!(vault.backup_if_due(24 * 60 * 60 * 1000).unwrap().is_none());
    // A zero max age forces one.
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(vault.backup_if_due(0).unwrap().is_some());
    let backups = inkveil_store::maintenance::list_backups(vault.paths()).unwrap();
    assert_eq!(backups.len(), 2);

    // A backup opens with the same password.
    let restored = tempfile::tempdir().unwrap();
    fs::copy(&backups[0].0, restored.path().join("vault.db")).unwrap();
    drop(vault);
    let vault = unlock(LockedVault::open(VaultPaths::new(restored.path())).unwrap());
    assert_eq!(vault.notes().unwrap().notes.len(), 1);
}

#[test]
fn backups_are_copied_to_another_folder() {
    let (_dir, vault) = new_vault();
    vault.create_note(&NoteMeta::new("n"), b"x").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let folder = elsewhere.path().join("Inkveil backups");
    let paths = vault.paths().clone();

    // Nothing to copy before the first backup.
    assert!(
        maintenance::copy_backups_to(&paths, &folder)
            .unwrap()
            .is_none()
    );
    vault.backup_if_due(0).unwrap();
    let copy = maintenance::copy_backups_to(&paths, &folder)
        .unwrap()
        .unwrap();
    // Copying again finds it there already.
    assert!(
        maintenance::copy_backups_to(&paths, &folder)
            .unwrap()
            .is_none()
    );
    // Other files in the folder are left alone; no temporary file stays behind.
    fs::write(folder.join("notes.txt"), b"mine").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    vault.backup_if_due(0).unwrap();
    maintenance::copy_backups_to(&paths, &folder)
        .unwrap()
        .unwrap();
    let mut names: Vec<String> = fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(names.contains(&"notes.txt".to_owned()));
    assert!(!names.iter().any(|n| n.contains(".tmp")));

    // A copy on its own opens with the same password.
    let restored = tempfile::tempdir().unwrap();
    fs::copy(&copy, restored.path().join("vault.db")).unwrap();
    drop(vault);
    let vault = unlock(LockedVault::open(VaultPaths::new(restored.path())).unwrap());
    assert_eq!(vault.notes().unwrap().notes.len(), 1);

    // The vault's own folder cannot be the destination.
    let err = maintenance::copy_backups_to(&paths, &paths.dir().join("copies")).unwrap_err();
    assert!(matches!(err, StoreError::Io(_)));
}

#[test]
fn history_thinning_keeps_recent_revisions() {
    let (_dir, mut vault) = new_vault();
    let id = vault.create_note(&NoteMeta::new("n"), b"x").unwrap();
    for _ in 0..25 {
        vault.snapshot_revision(&id).unwrap();
    }
    let removed = vault.thin_revisions(&id, inkveil_store::now_ms()).unwrap();
    // All 25 were made "today", so 20 newest + first are kept (the day is already covered).
    assert_eq!(removed, 4);
    assert_eq!(vault.revisions(&id).unwrap().len(), 21);
}

#[test]
fn header_with_too_many_slots_is_rejected() {
    let (_dir, mut vault) = new_vault();
    let key = SecretKey::generate().unwrap();
    for i in 0..20 {
        vault
            .add_platform_slot(&format!("p{i}"), Vec::new(), &key)
            .unwrap();
    }
    let paths = vault.paths().clone();
    drop(vault);
    assert!(matches!(
        LockedVault::open(paths),
        Err(StoreError::Corrupt(_))
    ));
}

#[test]
fn change_password() {
    let (_dir, mut vault) = new_vault();
    let id = vault.create_note(&NoteMeta::new("n"), b"kept").unwrap();
    vault
        .change_password(&Password::new("new password 2"), KdfParams::TESTING)
        .unwrap();

    let locked = reopen(vault);
    let failure = locked
        .unlock(Credential::Password(&Password::new(PASSWORD)))
        .unwrap_err();
    let vault = failure
        .vault
        .unlock(Credential::Password(&Password::new("new password 2")))
        .unwrap();
    assert_eq!(vault.load_body(&id).unwrap().as_slice(), b"kept");
}

#[test]
fn platform_slot_unlocks_and_can_be_removed() {
    let (_dir, mut vault) = new_vault();
    let platform_key = SecretKey::generate().unwrap();
    let slot = vault
        .add_platform_slot("windows-hello", b"challenge".to_vec(), &platform_key)
        .unwrap();

    let vault = reopen(vault)
        .unlock(Credential::PlatformKey {
            slot,
            key: &platform_key,
        })
        .unwrap();
    assert_eq!(vault.header().platform_slots("windows-hello").count(), 1);

    let mut vault = vault;
    vault.remove_slot(slot).unwrap();
    let failure = reopen(vault)
        .unlock(Credential::PlatformKey {
            slot,
            key: &platform_key,
        })
        .unwrap_err();
    assert!(matches!(failure.error, StoreError::NoSuchSlot));
}

#[test]
fn password_slot_cannot_be_removed() {
    let (_dir, mut vault) = new_vault();
    let password_slot = vault
        .header()
        .slots()
        .iter()
        .find(|s| matches!(s.kind, SlotKind::Password { .. }))
        .unwrap()
        .id;
    assert!(matches!(
        vault.remove_slot(password_slot),
        Err(StoreError::LastPasswordSlot)
    ));
}

#[test]
fn header_copy_rescues_a_damaged_database_header() {
    let (_dir, vault) = new_vault();
    let id = vault
        .create_note(&NoteMeta::new("n"), b"still here")
        .unwrap();
    let paths = vault.paths().clone();
    drop(vault);

    let conn = Connection::open(paths.database()).unwrap();
    conn.execute("UPDATE meta SET value = x'00' WHERE key = 'header'", [])
        .unwrap();
    drop(conn);

    let vault = unlock(LockedVault::open(paths.clone()).unwrap());
    assert_eq!(vault.load_body(&id).unwrap().as_slice(), b"still here");
    drop(vault);

    // The database header was repaired from the copy.
    fs::remove_file(paths.header()).unwrap();
    let vault = unlock(LockedVault::open(paths).unwrap());
    assert_eq!(vault.load_body(&id).unwrap().as_slice(), b"still here");
}

#[test]
fn tampered_or_swapped_records_are_detected() {
    let (_dir, vault) = new_vault();
    let a = vault.create_note(&NoteMeta::new("a"), b"note a").unwrap();
    let b = vault.create_note(&NoteMeta::new("b"), b"note b").unwrap();
    let paths = vault.paths().clone();
    drop(vault);

    let conn = Connection::open(paths.database()).unwrap();
    conn.execute(
        "UPDATE notes SET sealed_body = (SELECT sealed_body FROM notes WHERE id = ?1) WHERE id = ?2",
        [a.as_bytes().as_slice(), b.as_bytes().as_slice()],
    )
    .unwrap();
    drop(conn);

    let vault = unlock(LockedVault::open(paths).unwrap());
    assert!(matches!(vault.load_body(&b), Err(StoreError::Crypto(_))));
    assert_eq!(vault.load_body(&a).unwrap().as_slice(), b"note a");
}

#[test]
fn revisions_keep_history() {
    let (_dir, mut vault) = new_vault();
    let mut meta = NoteMeta::new("draft");
    let id = vault.create_note(&meta, b"v1").unwrap();
    let r1 = vault.snapshot_revision(&id).unwrap();
    meta.title = "final".into();
    vault.save_note(&id, &meta, b"v2").unwrap();
    let r2 = vault.snapshot_revision(&id).unwrap();

    let revs = vault.revisions(&id).unwrap();
    assert_eq!(revs.iter().map(|r| r.id).collect::<Vec<_>>(), [r1, r2]);
    assert_eq!(revs[0].meta.title, "draft");
    assert_eq!(vault.load_revision_body(&id, r1).unwrap().as_slice(), b"v1");

    let removed = vault.prune_revisions(&id, |r| r.id == r1).unwrap();
    assert_eq!(removed, 1);
    assert_eq!(vault.revisions(&id).unwrap().len(), 1);

    vault.delete_note_permanently(&id).unwrap();
    assert_eq!(vault.revisions(&id).unwrap().len(), 0);
    assert!(matches!(
        vault.load_body(&id),
        Err(StoreError::NoteNotFound(_))
    ));
}

#[test]
fn state_roundtrip() {
    let (_dir, vault) = new_vault();
    assert!(vault.get_state("session").unwrap().is_none());
    vault.put_state("session", b"tabs").unwrap();
    vault.put_state("session", b"tabs v2").unwrap();
    assert_eq!(
        vault.get_state("session").unwrap().unwrap().as_slice(),
        b"tabs v2"
    );
}

#[test]
fn backup_opens_with_same_password() {
    let (dir, vault) = new_vault();
    let id = vault
        .create_note(&NoteMeta::new("n"), b"backed up")
        .unwrap();
    assert!(vault.quick_check().unwrap());

    let backup_dir = dir.path().join("restored");
    fs::create_dir_all(&backup_dir).unwrap();
    let restored = VaultPaths::new(&backup_dir);
    vault.backup_to(&restored.database()).unwrap();
    drop(vault);

    let vault = unlock(LockedVault::open(restored).unwrap());
    assert_eq!(vault.load_body(&id).unwrap().as_slice(), b"backed up");
}

#[test]
fn lock_then_unlock_again() {
    let (_dir, vault) = new_vault();
    let id = vault.create_note(&NoteMeta::new("n"), b"x").unwrap();
    let vault = unlock(vault.lock());
    assert_eq!(vault.load_body(&id).unwrap().as_slice(), b"x");
}
