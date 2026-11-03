//! Encrypted binary attachments ("blobs"), used for images in notes.
//!
//! A note does not contain its images; it refers to them by [`BlobId`]. Each blob is sealed on its
//! own under the vault key, bound to its id. The id is random and never changes, so notes keep
//! pointing at the right image even after the vault key is replaced. To store a picture only
//! once when it is pasted again, each row also keeps a keyed fingerprint of the content; the key
//! is derived from the vault key, so the fingerprint reveals nothing about the picture without
//! it.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, params};
use zeroize::Zeroizing;

use crate::{
    StoreError, Vault, now_ms,
    record::{RecordCipher, RecordKind},
};

/// Largest blob accepted, in bytes.
pub const MAX_BLOB_BYTES: usize = 32 * 1024 * 1024;

/// Identifies a blob. Random, so it says nothing about the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobId(pub [u8; 16]);

impl BlobId {
    fn from_slice(bytes: &[u8]) -> Result<Self, StoreError> {
        bytes
            .try_into()
            .map(Self)
            .map_err(|_| StoreError::Corrupt("bad blob id"))
    }
}

impl Vault {
    /// Stores `data` encrypted and returns its id. Storing the same bytes again returns the
    /// existing id instead of a second copy.
    pub fn put_blob(&self, data: &[u8]) -> Result<BlobId, StoreError> {
        if data.len() > MAX_BLOB_BYTES {
            return Err(StoreError::Corrupt("attachment too large"));
        }
        let fingerprint = self.records.fingerprint(data);
        let existing: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT id FROM blobs WHERE fingerprint = ?1",
                [fingerprint.as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return BlobId::from_slice(&id);
        }
        let mut id = [0u8; 16];
        inkveil_crypto::fill_random(&mut id)?;
        let id = BlobId(id);
        let sealed = self.records.seal(RecordKind::Blob(&id.0), data)?;
        self.conn.execute(
            "INSERT INTO blobs (id, fingerprint, sealed, created_ms) VALUES (?1, ?2, ?3, ?4)",
            params![id.0.as_slice(), fingerprint.as_slice(), sealed, now_ms()],
        )?;
        Ok(id)
    }

    /// The decrypted bytes of a blob.
    pub fn load_blob(&self, id: BlobId) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        let sealed: Vec<u8> = self
            .conn
            .query_row(
                "SELECT sealed FROM blobs WHERE id = ?1",
                [id.0.as_slice()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(StoreError::Corrupt("missing attachment"))?;
        self.records.open(RecordKind::Blob(&id.0), &sealed)
    }

    /// Deletes blobs that nothing refers to any more. `in_use` must list every blob referenced
    /// by a note or a revision. Blobs younger than `min_age_ms` are kept, so an image pasted a
    /// moment ago is not removed before its note is saved.
    pub fn delete_unused_blobs(
        &self,
        in_use: &HashSet<BlobId>,
        min_age_ms: i64,
    ) -> Result<usize, StoreError> {
        let cutoff = now_ms() - min_age_ms;
        let rows: Vec<(Vec<u8>, i64)> = self
            .conn
            .prepare("SELECT id, created_ms FROM blobs")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut deleted = 0;
        for (id, created) in rows {
            let id = BlobId::from_slice(&id)?;
            if created < cutoff && !in_use.contains(&id) {
                self.conn
                    .execute("DELETE FROM blobs WHERE id = ?1", [id.0.as_slice()])?;
                deleted += 1;
            }
        }
        Ok(deleted)
    }
}

/// Re-seals every blob and recomputes its fingerprint when the vault key is replaced.
pub(crate) fn reseal_blobs(
    conn: &rusqlite::Connection,
    from: &RecordCipher,
    to: &RecordCipher,
) -> Result<(), StoreError> {
    let rows: Vec<(Vec<u8>, Vec<u8>)> = conn
        .prepare("SELECT id, sealed FROM blobs")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (id, sealed) in rows {
        let id = BlobId::from_slice(&id)?;
        let data = from.open(RecordKind::Blob(&id.0), &sealed)?;
        conn.execute(
            "UPDATE blobs SET sealed = ?2, fingerprint = ?3 WHERE id = ?1",
            params![
                id.0.as_slice(),
                to.seal(RecordKind::Blob(&id.0), &data)?,
                to.fingerprint(&data).as_slice()
            ],
        )?;
    }
    Ok(())
}
