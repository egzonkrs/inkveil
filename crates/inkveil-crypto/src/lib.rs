//! Cryptographic building blocks for Inkveil.
//!
//! Everything a vault stores is encrypted with keys from this crate:
//!
//! - [`SecretKey`] holds 256-bit key material and wipes it on drop.
//! - [`seal`] and [`open`] encrypt and authenticate data with XChaCha20-Poly1305.
//! - [`derive_password_key`] turns a [`Password`] into a key with Argon2id.
//! - [`RecoveryKey`] is a printable high-entropy code that can unlock a vault on its own.
//! - [`wrap_key`] and [`unwrap_key`] protect one key with another (used for key slots).
//!
//! The crate does no I/O apart from asking the operating system for random bytes.

#![forbid(unsafe_code)]

mod aead;
mod error;
mod kdf;
mod key;
mod password;
mod random;
mod recovery;

pub use aead::{NONCE_LEN, SEAL_OVERHEAD, TAG_LEN, open, seal};
pub use error::CryptoError;
pub use kdf::{KdfParams, SALT_LEN, Salt, derive_password_key, random_salt};
pub use key::{KEY_LEN, SecretKey, unwrap_key, wrap_key};
pub use password::{MIN_PASSWORD_CHARS, Password, PasswordPolicyError};
pub use random::fill_random;
pub use recovery::{RECOVERY_KEY_LEN, RecoveryKey, RecoveryKeyParseError};
