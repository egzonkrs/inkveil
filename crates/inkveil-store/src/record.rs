//! Note identifiers, note metadata and how records are sealed.

use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use inkveil_crypto::{SecretKey, fill_random, open, seal};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::StoreError;

/// Plaintext is padded to a multiple of this many bytes before sealing.
const PAD_BLOCK: usize = 256;

/// Current time in Unix milliseconds, or `None` if the system clock is before 1970.
pub fn now_ms_checked() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
}

/// Current time in Unix milliseconds (0 if the system clock is broken).
pub fn now_ms() -> i64 {
    now_ms_checked().unwrap_or(0)
}

/// Random 128-bit note id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NoteId([u8; 16]);

impl NoteId {
    /// A new random id.
    pub fn generate() -> Result<Self, StoreError> {
        let mut id = [0; 16];
        fill_random(&mut id)?;
        Ok(Self(id))
    }

    /// The raw id bytes.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    pub(crate) fn from_slice(bytes: &[u8]) -> Result<Self, StoreError> {
        bytes
            .try_into()
            .map(Self)
            .map_err(|_| StoreError::Corrupt("bad note id"))
    }
}

impl fmt::Display for NoteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl fmt::Debug for NoteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NoteId({self})")
    }
}

/// Id of one saved revision of a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RevisionId(pub(crate) i64);

/// Random id of one save. A note's metadata and body are sealed with the same save id, so the
/// halves of two different saves can never be combined.
pub(crate) type SaveId = [u8; 16];

pub(crate) fn new_save_id() -> Result<SaveId, StoreError> {
    let mut id = [0; 16];
    fill_random(&mut id)?;
    Ok(id)
}

pub(crate) fn save_id_from(bytes: Option<Vec<u8>>) -> Option<SaveId> {
    bytes.and_then(|b| b.try_into().ok())
}

/// Everything about a note except its body. Small enough to keep in memory for every note.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteMeta {
    /// Title shown in the sidebar and tab.
    pub title: String,
    /// Tags, in the order the user added them.
    pub tags: Vec<String>,
    /// Creation time, Unix milliseconds.
    pub created_ms: i64,
    /// Last modification time, Unix milliseconds.
    pub modified_ms: i64,
    /// When the note was moved to the trash, if it is there.
    pub trashed_ms: Option<i64>,
    /// Pinned notes are listed first.
    pub pinned: bool,
    /// The start of the note's text, shown in the sidebar and used as the title when the note has
    /// none.
    pub preview: String,
    /// The sidebar section the note is filed under, if any.
    pub section: Option<SectionId>,
}

/// Identifies a sidebar section. Section names are app state; notes only store the id.
pub type SectionId = [u8; 16];

/// Note metadata as written by format version 2 (before `section`).
#[derive(Serialize, Deserialize)]
struct NoteMetaV2 {
    title: String,
    tags: Vec<String>,
    created_ms: i64,
    modified_ms: i64,
    trashed_ms: Option<i64>,
    pinned: bool,
    preview: String,
}

impl From<NoteMetaV2> for NoteMeta {
    fn from(v2: NoteMetaV2) -> Self {
        Self {
            title: v2.title,
            tags: v2.tags,
            created_ms: v2.created_ms,
            modified_ms: v2.modified_ms,
            trashed_ms: v2.trashed_ms,
            pinned: v2.pinned,
            preview: v2.preview,
            section: None,
        }
    }
}

/// Note metadata as written by format version 1 (before `preview`). Kept so old notes still load.
#[derive(Serialize, Deserialize)]
struct NoteMetaV1 {
    title: String,
    tags: Vec<String>,
    created_ms: i64,
    modified_ms: i64,
    trashed_ms: Option<i64>,
    pinned: bool,
}

impl From<NoteMetaV1> for NoteMeta {
    fn from(v1: NoteMetaV1) -> Self {
        Self {
            title: v1.title,
            tags: v1.tags,
            created_ms: v1.created_ms,
            modified_ms: v1.modified_ms,
            trashed_ms: v1.trashed_ms,
            pinned: v1.pinned,
            preview: String::new(),
            section: None,
        }
    }
}

/// Versioned on-disk form of [`NoteMeta`]. Variants are never reordered or removed.
#[derive(Serialize, Deserialize)]
enum StoredMeta {
    V1(NoteMetaV1),
    V2(NoteMetaV2),
    V3(NoteMeta),
}

impl NoteMeta {
    /// Metadata for a new, empty note created now.
    pub fn new(title: impl Into<String>) -> Self {
        let now = now_ms();
        Self {
            title: title.into(),
            created_ms: now,
            modified_ms: now,
            ..Self::default()
        }
    }

    pub(crate) fn encode(&self) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        encode_zeroizing(&StoredMeta::V3(self.clone()))
            .ok_or(StoreError::Corrupt("note metadata could not be encoded"))
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        match postcard::from_bytes::<StoredMeta>(bytes) {
            Ok(StoredMeta::V1(meta)) => Ok(meta.into()),
            Ok(StoredMeta::V2(meta)) => Ok(meta.into()),
            Ok(StoredMeta::V3(meta)) => Ok(meta),
            Err(_) => Err(StoreError::Corrupt("note metadata could not be read")),
        }
    }
}

/// Serializes into a buffer that is wiped on drop, including every intermediate buffer.
pub(crate) fn encode_zeroizing<T: Serialize>(value: &T) -> Option<Zeroizing<Vec<u8>>> {
    let mut capacity = 1024;
    loop {
        let mut buffer = Zeroizing::new(vec![0u8; capacity]);
        match postcard::to_slice(value, &mut buffer) {
            Ok(used) => {
                let len = used.len();
                buffer.truncate(len);
                return Some(buffer);
            }
            Err(postcard::Error::SerializeBufferFull) if capacity < 256 << 20 => capacity *= 4,
            Err(_) => return None,
        }
    }
}

/// A note as listed in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteSummary {
    /// The note.
    pub id: NoteId,
    /// Its metadata.
    pub meta: NoteMeta,
}

/// All notes, plus the ids of any that could not be read (damaged or tampered with).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteListing {
    /// Notes that decrypted correctly.
    pub notes: Vec<NoteSummary>,
    /// Notes that failed to decrypt. They stay in the vault untouched.
    pub unreadable: Vec<NoteId>,
}

/// One saved revision of a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionSummary {
    /// The revision.
    pub id: RevisionId,
    /// Note metadata at the time of the revision.
    pub meta: NoteMeta,
}

/// What a sealed record belongs to. Bound into the ciphertext as associated data, so a record
/// only decrypts in the exact place it was written for.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RecordKind<'a> {
    NoteMeta {
        id: &'a NoteId,
        save: &'a SaveId,
    },
    NoteBody {
        id: &'a NoteId,
        save: &'a SaveId,
    },
    RevisionMeta {
        id: &'a NoteId,
        save: &'a SaveId,
    },
    RevisionBody {
        id: &'a NoteId,
        save: &'a SaveId,
    },
    State(&'a str),
    /// An attachment, bound to its blob id.
    Blob(&'a [u8; 16]),
    /// Records written by format 1, readable only while upgrading a vault.
    LegacyNoteMeta(&'a NoteId),
    LegacyNoteBody(&'a NoteId),
}

impl RecordKind<'_> {
    fn aad(&self, vault_id: &[u8; 16]) -> Vec<u8> {
        let (label, id, save): (&[u8], &[u8], &[u8]) = match self {
            Self::NoteMeta { id, save } => (b"inkveil/v2/note-meta", id.as_bytes(), *save),
            Self::NoteBody { id, save } => (b"inkveil/v2/note-body", id.as_bytes(), *save),
            Self::RevisionMeta { id, save } => (b"inkveil/v2/rev-meta", id.as_bytes(), *save),
            Self::RevisionBody { id, save } => (b"inkveil/v2/rev-body", id.as_bytes(), *save),
            Self::State(key) => (b"inkveil/v1/state", key.as_bytes(), &[]),
            Self::Blob(id) => (b"inkveil/v1/blob", id.as_slice(), &[]),
            Self::LegacyNoteMeta(id) => (b"inkveil/v1/note-meta", id.as_bytes(), &[]),
            Self::LegacyNoteBody(id) => (b"inkveil/v1/note-body", id.as_bytes(), &[]),
        };
        let mut aad = Vec::with_capacity(label.len() + 16 + id.len() + save.len());
        aad.extend_from_slice(label);
        aad.extend_from_slice(vault_id);
        aad.extend_from_slice(id);
        aad.extend_from_slice(save);
        aad
    }
}

/// Seals records for one vault.
pub(crate) struct RecordCipher {
    key: SecretKey,
    fingerprint_key: SecretKey,
    vault_id: [u8; 16],
}

impl RecordCipher {
    pub(crate) fn new(vault_key: &SecretKey, vault_id: [u8; 16]) -> Self {
        Self {
            key: vault_key.derive("inkveil/v1/records"),
            fingerprint_key: vault_key.derive("inkveil/v1/blob-fingerprint"),
            vault_id,
        }
    }

    /// A keyed fingerprint of attachment content, for storing each picture once.
    pub(crate) fn fingerprint(&self, data: &[u8]) -> [u8; 32] {
        self.fingerprint_key.fingerprint(data)
    }

    pub(crate) fn seal(
        &self,
        kind: RecordKind<'_>,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, StoreError> {
        let padded = pad(plaintext)?;
        Ok(seal(&self.key, &padded, &kind.aad(&self.vault_id))?)
    }

    pub(crate) fn open(
        &self,
        kind: RecordKind<'_>,
        sealed: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        let padded = open(&self.key, sealed, &kind.aad(&self.vault_id))?;
        unpad(&padded)
    }
}

impl fmt::Debug for RecordCipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordCipher").finish_non_exhaustive()
    }
}

/// `len (u32 LE) || data || zeros`, rounded up to a multiple of [`PAD_BLOCK`].
fn pad(data: &[u8]) -> Result<Zeroizing<Vec<u8>>, StoreError> {
    let len = u32::try_from(data.len()).map_err(|_| StoreError::Corrupt("record too large"))?;
    let total = (4 + data.len()).div_ceil(PAD_BLOCK) * PAD_BLOCK;
    let mut out = Zeroizing::new(Vec::with_capacity(total));
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(data);
    out.resize(total, 0);
    Ok(out)
}

fn unpad(padded: &[u8]) -> Result<Zeroizing<Vec<u8>>, StoreError> {
    let (len, rest) = padded
        .split_first_chunk::<4>()
        .ok_or(StoreError::Corrupt("record too short"))?;
    let len = usize::try_from(u32::from_le_bytes(*len))
        .map_err(|_| StoreError::Corrupt("record length"))?;
    rest.get(..len)
        .map(|data| Zeroizing::new(data.to_vec()))
        .ok_or(StoreError::Corrupt("record length"))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn cipher() -> RecordCipher {
        RecordCipher::new(&SecretKey::from_bytes([5; 32]), [1; 16])
    }

    #[test]
    fn sizes_are_bucketed() {
        let c = cipher();
        let id = NoteId([0; 16]);
        let save = [9; 16];
        let short = c
            .seal(
                RecordKind::NoteBody {
                    id: &id,
                    save: &save,
                },
                b"a",
            )
            .unwrap();
        let longer = c
            .seal(
                RecordKind::NoteBody {
                    id: &id,
                    save: &save,
                },
                &[b'a'; 200],
            )
            .unwrap();
        assert_eq!(short.len(), longer.len());
    }

    #[test]
    fn records_are_bound_to_their_place_and_save() {
        let c = cipher();
        let a = NoteId([1; 16]);
        let b = NoteId([2; 16]);
        let (s1, s2) = ([1; 16], [2; 16]);
        let sealed = c
            .seal(RecordKind::NoteBody { id: &a, save: &s1 }, b"secret")
            .unwrap();
        // Another note, another save, the meta slot, the revision slot and the legacy format
        // all refuse it.
        assert!(
            c.open(RecordKind::NoteBody { id: &b, save: &s1 }, &sealed)
                .is_err()
        );
        assert!(
            c.open(RecordKind::NoteBody { id: &a, save: &s2 }, &sealed)
                .is_err()
        );
        assert!(
            c.open(RecordKind::NoteMeta { id: &a, save: &s1 }, &sealed)
                .is_err()
        );
        assert!(
            c.open(RecordKind::RevisionBody { id: &a, save: &s1 }, &sealed)
                .is_err()
        );
        assert!(c.open(RecordKind::LegacyNoteBody(&a), &sealed).is_err());
        assert_eq!(
            c.open(RecordKind::NoteBody { id: &a, save: &s1 }, &sealed)
                .unwrap()
                .as_slice(),
            b"secret"
        );
    }

    #[test]
    fn meta_roundtrip() {
        let meta = NoteMeta {
            title: "Groceries".into(),
            tags: vec!["home".into()],
            preview: "Milk".into(),
            ..NoteMeta::new("")
        };
        assert_eq!(NoteMeta::decode(&meta.encode().unwrap()).unwrap(), meta);
    }

    #[test]
    fn version_1_metadata_still_loads() {
        let v1 = postcard::to_stdvec(&StoredMeta::V1(NoteMetaV1 {
            title: "Old".into(),
            tags: vec!["t".into()],
            created_ms: 1,
            modified_ms: 2,
            trashed_ms: None,
            pinned: true,
        }))
        .unwrap();
        let meta = NoteMeta::decode(&v1).unwrap();
        assert_eq!(meta.title, "Old");
        assert!(meta.pinned);
        assert_eq!(meta.preview, "");
    }

    #[test]
    fn version_2_metadata_still_loads() {
        let v2 = postcard::to_stdvec(&StoredMeta::V2(NoteMetaV2 {
            title: "Two".into(),
            tags: Vec::new(),
            created_ms: 1,
            modified_ms: 2,
            trashed_ms: None,
            pinned: false,
            preview: "first line".into(),
        }))
        .unwrap();
        let meta = NoteMeta::decode(&v2).unwrap();
        assert_eq!(meta.preview, "first line");
        assert_eq!(meta.section, None);
    }

    #[test]
    fn zeroizing_encoder_handles_large_values() {
        let big = NoteMeta {
            preview: "x".repeat(100_000),
            ..NoteMeta::default()
        };
        let encoded = encode_zeroizing(&StoredMeta::V3(big.clone())).unwrap();
        assert_eq!(NoteMeta::decode(&encoded).unwrap(), big);
    }

    proptest! {
        #[test]
        fn pad_roundtrip(data in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let padded = pad(&data).unwrap();
            prop_assert_eq!(padded.len() % PAD_BLOCK, 0);
            let unpadded = unpad(&padded).unwrap();
            prop_assert_eq!(unpadded.as_slice(), data.as_slice());
        }
    }
}
