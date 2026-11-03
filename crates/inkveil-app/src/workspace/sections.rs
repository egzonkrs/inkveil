//! Sidebar sections: named, collapsible groups of notes.
//!
//! The list of sections (names, order, collapsed state) is app state stored encrypted in the
//! vault; each note only records the id of its section.

use inkveil_store::SectionId;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub id: SectionId,
    pub name: String,
    pub collapsed: bool,
}

#[derive(Serialize, Deserialize)]
enum Stored {
    V1(Vec<Section>),
}

pub fn encode(sections: &[Section]) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(postcard::to_stdvec(&Stored::V1(sections.to_vec())).unwrap_or_default())
}

/// Stored sections, or none if they cannot be read. Notes keep their section ids either way, so
/// nothing is lost: their notes just show as unsorted until a section is recreated.
pub fn decode(bytes: &[u8]) -> Vec<Section> {
    match postcard::from_bytes::<Stored>(bytes) {
        Ok(Stored::V1(sections)) => sections,
        Err(_) => Vec::new(),
    }
}

pub fn new_id() -> SectionId {
    let mut id = [0; 16];
    // A failure here would mean the OS random generator is broken; fall back to the clock so the
    // UI keeps working (ids only need to be unique, not secret).
    if inkveil_crypto::fill_random(&mut id).is_err() {
        id[..8].copy_from_slice(&inkveil_store::now_ms().to_le_bytes());
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let sections = vec![
            Section {
                id: [1; 16],
                name: "Work".into(),
                collapsed: false,
            },
            Section {
                id: [2; 16],
                name: "Personal".into(),
                collapsed: true,
            },
        ];
        assert_eq!(decode(&encode(&sections)), sections);
        assert_eq!(decode(b"garbage"), Vec::<Section>::new());
        assert_ne!(new_id(), new_id());
    }
}
