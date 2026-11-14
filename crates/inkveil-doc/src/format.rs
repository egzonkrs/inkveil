//! The stored form of a document.
//!
//! Documents are encoded with postcard inside a versioned enum. When the model changes, the old
//! layout keeps its variant and gains a migration, so notes written by any release stay readable.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{Block, Document};

/// A stored document could not be read.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The bytes are not a document this version understands.
    #[error("the note content is damaged or was written by a newer version of Inkveil")]
    Invalid,
}

#[derive(Serialize, Deserialize)]
enum StoredDoc {
    V1(Vec<Block>),
}

/// [`StoredDoc`] for writing: the same bytes, from blocks that are only borrowed, so encoding
/// does not copy the document first. Its variants must stay in step with [`StoredDoc`].
#[derive(Serialize)]
enum StoredDocRef<'a> {
    V1(Vec<&'a Block>),
}

impl Document {
    /// Encodes the document for storage, into a buffer that is wiped when dropped (as is every
    /// intermediate buffer).
    ///
    /// # Panics
    ///
    /// If the document is larger than 1 GiB encoded.
    pub fn encode(&self) -> Zeroizing<Vec<u8>> {
        let stored = StoredDocRef::V1(self.blocks.iter().map(AsRef::as_ref).collect());
        // Roomy enough for most notes on the first try: the text, and as much again for its
        // formatting and structure. The loop below grows it when that is not enough.
        let text: usize = self.blocks.iter().map(|block| text_len(block)).sum();
        let mut capacity = (text * 2 + self.blocks.len() * 64).max(4096);
        loop {
            let mut buffer = Zeroizing::new(vec![0u8; capacity]);
            match postcard::to_slice(&stored, &mut buffer) {
                Ok(used) => {
                    let len = used.len();
                    buffer.truncate(len);
                    return buffer;
                }
                Err(postcard::Error::SerializeBufferFull) if capacity < 1 << 30 => capacity *= 4,
                Err(e) => panic!("encoding a document failed: {e}"),
            }
        }
    }

    /// Decodes a stored document. Block structure is repaired; damaged text is rejected.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        match postcard::from_bytes::<StoredDoc>(bytes) {
            Ok(StoredDoc::V1(blocks)) => {
                let doc = Self::from_blocks(blocks);
                if doc.is_valid() {
                    Ok(doc)
                } else {
                    Err(DecodeError::Invalid)
                }
            }
            Err(_) => Err(DecodeError::Invalid),
        }
    }
}

/// How many bytes of text a block holds.
fn text_len(block: &Block) -> usize {
    match block {
        Block::Text(t) => t.text.as_str().len(),
        Block::Divider | Block::Image(_) => 0,
        Block::Table(table) => table
            .rows()
            .iter()
            .flatten()
            .map(|cell| cell.as_str().len())
            .sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Marks, RichText, Table, TextBlock, TextKind};

    #[test]
    fn encoding_writes_exactly_the_stored_layout() {
        // Notes on disk were written from `StoredDoc`; the borrowed form must not differ by
        // a single byte, for small notes and for ones that need a bigger buffer.
        for text in ["", "short", &"a long line of text. ".repeat(4000)] {
            let mut doc = Document::from_plain_text(text);
            doc.blocks.push(std::sync::Arc::new(Block::Divider));
            let owned: Vec<Block> = doc.blocks.iter().map(|b| b.as_ref().clone()).collect();
            let expected = postcard::to_stdvec(&StoredDoc::V1(owned)).unwrap();
            assert_eq!(doc.encode().as_slice(), expected.as_slice());
        }
    }

    #[test]
    fn roundtrip() {
        let mut text = RichText::plain("Hello ");
        text.insert(
            6,
            "world",
            Marks {
                bold: true,
                link: Some("https://example.com".into()),
                ..Marks::NONE
            },
        );
        let doc = Document::from_blocks([
            Block::Text(TextBlock {
                kind: TextKind::Heading(1),
                ..TextBlock::paragraph(text)
            }),
            Block::Divider,
            Block::Table(Table::new(2, 3)),
        ]);
        assert_eq!(Document::decode(&doc.encode()).unwrap(), doc);
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(Document::decode(&[0xFF, 0x01]), Err(DecodeError::Invalid));
    }
}
