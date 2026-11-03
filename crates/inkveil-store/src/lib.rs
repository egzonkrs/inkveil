//! Encrypted, crash-safe storage for Inkveil vaults.
//!
//! A vault is a folder holding:
//!
//! - `vault.db`: a SQLite database in WAL mode with full fsync. Every save is one transaction, so
//!   a crash or power cut leaves either the old or the new version of a note, never a mix.
//! - `vault.header`: a copy of the key header, so a damaged database page cannot lock you out.
//! - `vault.lock`: held while the vault is open, so two app instances never write at once.
//! - `backups/`: encrypted backup copies of the database.
//!
//! Every note title, tag, body and piece of app state is sealed with XChaCha20-Poly1305 under a
//! key derived from the vault key. The associated data binds each record to its vault and row, so
//! records cannot be swapped or replayed into another place. Plaintext is padded to 256-byte
//! blocks before sealing to hide exact sizes.
//!
//! The vault key itself is stored only in wrapped form, once per unlock method ("key slot").

#![forbid(unsafe_code)]

mod blobs;
mod db;
mod error;
mod fsutil;
mod header;
mod instance_lock;
pub mod maintenance;
mod paths;
mod record;
mod throttle;
mod vault;

pub use blobs::{BlobId, MAX_BLOB_BYTES};
pub use error::StoreError;
pub use header::{KeySlot, SlotId, SlotKind, VaultHeader};
pub use paths::VaultPaths;
pub use record::{
    NoteId, NoteListing, NoteMeta, NoteSummary, RevisionId, RevisionSummary, SectionId, now_ms,
    now_ms_checked,
};
pub use throttle::{Attempts, FREE_ATTEMPTS, wait_after};
pub use vault::{Credential, LockedVault, UnlockFailure, Vault};
