use std::fmt;

use hkdf::Hkdf;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{CryptoError, aead, random::fill_random};

/// Length of every symmetric key, in bytes.
pub const KEY_LEN: usize = 32;

/// 256-bit secret key material.
///
/// The bytes are wiped when the key is dropped. The type is intentionally not `Clone` so that
/// copies of a key are explicit, and its `Debug` output never shows the bytes.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretKey([u8; KEY_LEN]);

impl SecretKey {
    /// Generates a new random key.
    pub fn generate() -> Result<Self, CryptoError> {
        let mut key = Self([0; KEY_LEN]);
        fill_random(&mut key.0)?;
        Ok(key)
    }

    /// Wraps existing key bytes. The caller should wipe its own copy afterwards.
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Returns the raw key bytes. Keep the borrow short-lived and never log it.
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// Derives an independent key for one purpose with HKDF-SHA256.
    ///
    /// Different `purpose` strings give unrelated keys, so a key used for one job can never be
    /// used to attack another.
    #[must_use]
    pub fn derive(&self, purpose: &str) -> Self {
        Self::derive_from(&self.0, None, purpose.as_bytes())
    }

    /// HMAC-SHA256 of `data` under this key: a fingerprint that can only be computed, or
    /// compared, by someone who has the key.
    pub fn fingerprint(&self, data: &[u8]) -> [u8; 32] {
        // HKDF-Extract with this key as the salt is exactly HMAC-SHA256(key, data).
        let (prk, _) = Hkdf::<Sha256>::extract(Some(&self.0), data);
        prk.into()
    }

    /// HKDF-SHA256 from arbitrary input key material.
    pub(crate) fn derive_from(ikm: &[u8], salt: Option<&[u8]>, info: &[u8]) -> Self {
        let mut out = Self([0; KEY_LEN]);
        Hkdf::<Sha256>::new(salt, ikm)
            .expand(info, &mut out.0)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        out
    }
}

impl PartialEq for SecretKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl Eq for SecretKey {}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(<redacted>)")
    }
}

/// Encrypts `key` under `wrapping_key`, bound to `context`.
///
/// The same `context` must be given to [`unwrap_key`]. Use it to tie the wrapped key to where it
/// is stored (for example a vault id and slot id) so it cannot be moved somewhere else.
pub fn wrap_key(
    wrapping_key: &SecretKey,
    key: &SecretKey,
    context: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    aead::seal(wrapping_key, key.as_bytes(), context)
}

/// Reverses [`wrap_key`]. Fails with [`CryptoError::Decrypt`] for a wrong key or context.
pub fn unwrap_key(
    wrapping_key: &SecretKey,
    wrapped: &[u8],
    context: &[u8],
) -> Result<SecretKey, CryptoError> {
    let plain = aead::open(wrapping_key, wrapped, context)?;
    let bytes: [u8; KEY_LEN] = plain
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::InvalidFormat)?;
    Ok(SecretKey::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_differ() {
        let a = SecretKey::generate().unwrap();
        let b = SecretKey::generate().unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn derive_is_deterministic_and_purpose_separated() {
        let master = SecretKey::from_bytes([7; KEY_LEN]);
        assert_eq!(master.derive("notes"), master.derive("notes"));
        assert_ne!(master.derive("notes"), master.derive("state"));
        assert_ne!(master.derive("notes"), master);
    }

    #[test]
    fn debug_never_shows_bytes() {
        let key = SecretKey::from_bytes([0xAB; KEY_LEN]);
        let shown = format!("{key:?}");
        assert!(!shown.contains("171") && !shown.to_lowercase().contains("ab"));
    }

    #[test]
    fn wrap_roundtrip_and_context_binding() {
        let kek = SecretKey::generate().unwrap();
        let key = SecretKey::generate().unwrap();
        let wrapped = wrap_key(&kek, &key, b"slot-1").unwrap();
        assert_eq!(unwrap_key(&kek, &wrapped, b"slot-1").unwrap(), key);
        assert_eq!(
            unwrap_key(&kek, &wrapped, b"slot-2"),
            Err(CryptoError::Decrypt)
        );
        let other = SecretKey::generate().unwrap();
        assert_eq!(
            unwrap_key(&other, &wrapped, b"slot-1"),
            Err(CryptoError::Decrypt)
        );
    }
}
