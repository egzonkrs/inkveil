use std::{io, time::Duration};

use inkveil_crypto::{CryptoError, PasswordPolicyError};
use thiserror::Error;

use crate::NoteId;

/// Errors from opening, unlocking or using a vault.
#[derive(Debug, Error)]
pub enum StoreError {
    /// A file system operation failed.
    #[error("file system error: {0}")]
    Io(#[from] io::Error),
    /// SQLite reported an error.
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    /// Encryption or decryption failed.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// The password, recovery key or platform key did not unlock the vault.
    #[error("the password or key is not correct")]
    WrongCredential,
    /// Too many wrong unlock attempts in a row; the next one is allowed after this wait.
    #[error("too many wrong attempts; try again in {} seconds", .0.as_secs().max(1))]
    TooManyAttempts(Duration),
    /// A new password does not meet the password rules.
    #[error(transparent)]
    PasswordPolicy(#[from] PasswordPolicyError),
    /// The vault has no key slot of the requested kind or id.
    #[error("this unlock method is not set up for this vault")]
    NoSuchSlot,
    /// Removing the slot would leave no way to unlock the vault.
    #[error("the last password slot cannot be removed")]
    LastPasswordSlot,
    /// Another Inkveil window or process already has this vault open.
    #[error("this vault is already open in another Inkveil window")]
    AlreadyOpen,
    /// A vault already exists where a new one was to be created.
    #[error("a vault already exists in this folder")]
    AlreadyExists,
    /// The folder has no vault database but still holds a vault's key header or backups.
    #[error(
        "this folder still holds the key header or backups of a vault whose database is missing"
    )]
    VaultFilesRemain,
    /// No vault exists in the given folder.
    #[error("no vault was found in this folder")]
    NotFound,
    /// The note does not exist.
    #[error("note {0} does not exist")]
    NoteNotFound(NoteId),
    /// Stored data is damaged or not in a format this version understands.
    #[error("vault data is damaged: {0}")]
    Corrupt(&'static str),
    /// The vault was written by a newer version of Inkveil.
    #[error("this vault needs a newer version of Inkveil (format {0})")]
    UnsupportedVersion(u32),
}
