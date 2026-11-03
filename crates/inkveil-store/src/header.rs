//! The vault key header: which unlock methods exist and the vault key wrapped for each.

use inkveil_crypto::{KdfParams, Salt, SecretKey, unwrap_key, wrap_key};
use serde::{Deserialize, Serialize};

use crate::StoreError;

const MAGIC: &[u8; 8] = b"INKVEIL\0";
/// More slots than any real vault needs; caps the work a tampered header can cause.
const MAX_SLOTS: usize = 16;
/// Largest header accepted, in bytes.
pub(crate) const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Identifies a key slot within one vault.
pub type SlotId = u32;

/// How the key that wraps the vault key is obtained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlotKind {
    /// Argon2id over the user's password.
    Password {
        /// Random salt for this slot.
        salt: Salt,
        /// Argon2id cost.
        kdf: KdfParams,
    },
    /// HKDF over a printed recovery key.
    Recovery {
        /// Random salt for this slot.
        salt: Salt,
    },
    /// A key released by the operating system, such as Windows Hello.
    Platform {
        /// Which provider created the slot, e.g. `"windows-hello"`.
        provider: String,
        /// Provider-specific public data (for Windows Hello, the challenge to sign).
        data: Vec<u8>,
    },
}

/// One way to unlock the vault: the vault key encrypted under a key from `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeySlot {
    /// Slot id, unique within the vault.
    pub id: SlotId,
    /// Where the wrapping key comes from.
    pub kind: SlotKind,
    wrapped_key: Vec<u8>,
}

/// The public, unencrypted part of a vault. It reveals which unlock methods exist, nothing more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultHeader {
    /// Random id of this vault, bound into every key slot and record.
    pub vault_id: [u8; 16],
    /// Creation time in Unix milliseconds.
    pub created_ms: i64,
    slots: Vec<KeySlot>,
}

/// Versioned on-disk form. A new header layout becomes a new variant with a migration.
#[derive(Serialize, Deserialize)]
enum StoredHeader {
    V1(VaultHeader),
}

impl VaultHeader {
    pub(crate) fn new(vault_id: [u8; 16], created_ms: i64) -> Self {
        Self {
            vault_id,
            created_ms,
            slots: Vec::new(),
        }
    }

    /// All key slots.
    pub fn slots(&self) -> &[KeySlot] {
        &self.slots
    }

    /// The slot with this id.
    pub fn slot(&self, id: SlotId) -> Option<&KeySlot> {
        self.slots.iter().find(|s| s.id == id)
    }

    /// Slots created by a platform provider such as `"windows-hello"`.
    pub fn platform_slots<'a>(&'a self, provider: &'a str) -> impl Iterator<Item = &'a KeySlot> {
        self.slots.iter().filter(
            move |s| matches!(&s.kind, SlotKind::Platform { provider: p, .. } if p == provider),
        )
    }

    /// Whether a recovery key slot exists.
    pub fn has_recovery_key(&self) -> bool {
        self.slots
            .iter()
            .any(|s| matches!(s.kind, SlotKind::Recovery { .. }))
    }

    /// Adds a slot holding `vault_key` wrapped under `wrapping_key`, returning its id.
    pub(crate) fn add_slot(
        &mut self,
        kind: SlotKind,
        wrapping_key: &SecretKey,
        vault_key: &SecretKey,
    ) -> Result<SlotId, StoreError> {
        let id = self.slots.iter().map(|s| s.id).max().map_or(1, |m| m + 1);
        let wrapped_key = wrap_key(wrapping_key, vault_key, &self.slot_context(id))?;
        self.slots.push(KeySlot {
            id,
            kind,
            wrapped_key,
        });
        Ok(id)
    }

    pub(crate) fn remove_slots(&mut self, mut remove: impl FnMut(&KeySlot) -> bool) {
        self.slots.retain(|s| !remove(s));
    }

    /// Unwraps the vault key from `slot` using `wrapping_key`.
    pub(crate) fn unwrap(
        &self,
        slot: &KeySlot,
        wrapping_key: &SecretKey,
    ) -> Result<SecretKey, StoreError> {
        unwrap_key(wrapping_key, &slot.wrapped_key, &self.slot_context(slot.id))
            .map_err(|_| StoreError::WrongCredential)
    }

    fn slot_context(&self, id: SlotId) -> Vec<u8> {
        let mut ctx = b"inkveil/v1/key-slot".to_vec();
        ctx.extend_from_slice(&self.vault_id);
        ctx.extend_from_slice(&id.to_le_bytes());
        ctx
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, StoreError> {
        let mut out = MAGIC.to_vec();
        let body = postcard::to_stdvec(&StoredHeader::V1(self.clone()))
            .map_err(|_| StoreError::Corrupt("header could not be encoded"))?;
        out.extend_from_slice(&body);
        Ok(out)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        let body = bytes
            .strip_prefix(MAGIC)
            .ok_or(StoreError::Corrupt("not an Inkveil header"))?;
        if bytes.len() > MAX_HEADER_BYTES {
            return Err(StoreError::Corrupt("header too large"));
        }
        let Ok(StoredHeader::V1(header)) = postcard::from_bytes::<StoredHeader>(body) else {
            return Err(StoreError::Corrupt("header could not be read"));
        };
        let password_slots = header
            .slots
            .iter()
            .filter(|s| matches!(s.kind, SlotKind::Password { .. }))
            .count();
        if header.slots.len() > MAX_SLOTS || password_slots > 1 {
            return Err(StoreError::Corrupt("header has too many key slots"));
        }
        Ok(header)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let mut header = VaultHeader::new([9; 16], 1234);
        let kek = SecretKey::generate().unwrap();
        let vault_key = SecretKey::generate().unwrap();
        header
            .add_slot(SlotKind::Recovery { salt: [3; 16] }, &kek, &vault_key)
            .unwrap();
        let decoded = VaultHeader::decode(&header.encode().unwrap()).unwrap();
        assert_eq!(decoded, header);
        assert_eq!(decoded.unwrap(&decoded.slots[0], &kek).unwrap(), vault_key);
    }

    #[test]
    fn slot_cannot_move_between_vaults() {
        let kek = SecretKey::generate().unwrap();
        let vault_key = SecretKey::generate().unwrap();
        let mut a = VaultHeader::new([1; 16], 0);
        a.add_slot(SlotKind::Recovery { salt: [0; 16] }, &kek, &vault_key)
            .unwrap();
        let mut b = VaultHeader::new([2; 16], 0);
        b.slots = a.slots.clone();
        assert!(matches!(
            b.unwrap(&b.slots[0], &kek),
            Err(StoreError::WrongCredential)
        ));
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(VaultHeader::decode(b"nope").is_err());
        assert!(VaultHeader::decode(b"INKVEIL\0\xff\xff\xff").is_err());
    }
}
