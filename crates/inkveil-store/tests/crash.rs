//! Kills a writing process at random moments and checks that the vault is never damaged.
//!
//! The test re-runs its own binary as a child that saves notes in a tight loop. The parent kills
//! it mid-write, reopens the vault and checks that every note decrypts, and that each note's
//! title and body come from the same save (meta and body are written in one row update, so a torn
//! write would show up as a mismatch).

#![allow(clippy::unwrap_used)]

use std::{
    env,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use inkveil_crypto::{KdfParams, Password};
use inkveil_store::{Credential, LockedVault, NoteId, NoteMeta, Vault, VaultPaths};

const PASSWORD: &str = "crash test 1";
const CHILD_ENV: &str = "INKVEIL_CRASH_CHILD_VAULT";
const NOTES: usize = 8;

fn body_for(counter: u64) -> Vec<u8> {
    // Vary the size so saves cross page boundaries.
    let len = 100 + usize::try_from(counter % 50).unwrap() * 300;
    counter.to_le_bytes().repeat(len / 8)
}

/// The child half: not a real test unless the parent set the environment variable.
#[test]
fn crash_child() {
    let Ok(dir) = env::var(CHILD_ENV) else {
        return;
    };
    let paths = VaultPaths::new(PathBuf::from(dir));
    let vault = LockedVault::open(paths)
        .unwrap()
        .unlock(Credential::Password(&Password::new(PASSWORD)))
        .unwrap();
    let ids: Vec<NoteId> = vault
        .notes()
        .unwrap()
        .notes
        .into_iter()
        .map(|n| n.id)
        .collect();
    let mut counter: u64 = 0;
    loop {
        for id in &ids {
            counter += 1;
            let meta = NoteMeta::new(counter.to_string());
            vault.save_note(id, &meta, &body_for(counter)).unwrap();
            if counter.is_multiple_of(7) {
                vault.snapshot_revision(id).unwrap();
            }
        }
    }
}

#[test]
fn killing_the_writer_never_corrupts_the_vault() {
    if env::var(CHILD_ENV).is_ok() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let paths = VaultPaths::new(dir.path().join("vault"));
    {
        let vault =
            Vault::create(paths.clone(), &Password::new(PASSWORD), KdfParams::TESTING).unwrap();
        for _ in 0..NOTES {
            vault
                .create_note(&NoteMeta::new("0"), &body_for(0))
                .unwrap();
        }
    }

    let exe = env::current_exe().unwrap();
    let mut highest_counter = 0;
    for round in 0..12u64 {
        let mut child = Command::new(&exe)
            .args(["crash_child", "--exact", "--nocapture"])
            .env(CHILD_ENV, paths.dir())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        thread::sleep(Duration::from_millis(150 + round * 37));
        child.kill().unwrap();
        child.wait().unwrap();

        let vault = LockedVault::open(paths.clone())
            .unwrap()
            .unlock(Credential::Password(&Password::new(PASSWORD)))
            .unwrap();
        assert!(
            vault.quick_check().unwrap(),
            "round {round}: integrity check failed"
        );
        let listing = vault.notes().unwrap();
        assert!(
            listing.unreadable.is_empty(),
            "round {round}: unreadable notes"
        );
        let notes = listing.notes;
        assert_eq!(notes.len(), NOTES);
        for note in notes {
            let counter: u64 = note.meta.title.parse().unwrap();
            highest_counter = highest_counter.max(counter);
            let body = vault.load_body(&note.id).unwrap();
            assert_eq!(
                body.as_slice(),
                body_for(counter).as_slice(),
                "round {round}: title and body come from different saves"
            );
            for rev in vault.revisions(&note.id).unwrap() {
                vault.load_revision_body(&note.id, rev.id).unwrap();
            }
        }
    }
    // Guard against a vacuous pass: the child must actually have been writing.
    assert!(
        highest_counter > 0,
        "the child process never saved anything"
    );
}
