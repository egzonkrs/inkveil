//! Open tabs and caret positions, restored after unlocking.

use inkveil_store::NoteId;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::tab_layout::Split;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTab {
    pub note: NoteId,
    pub caret_block: u32,
    pub caret_offset: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub tabs: Vec<OpenTab>,
    pub active: Option<u32>,
    pub pinned: Vec<NoteId>,
    pub splits: Vec<Split<NoteId>>,
}

/// A session as stored before tabs could be pinned or split.
#[derive(Serialize, Deserialize)]
struct SessionV1 {
    tabs: Vec<OpenTab>,
    active: Option<u32>,
}

// New versions go at the end: the variant's position is what is stored.
#[derive(Serialize, Deserialize)]
enum Stored {
    V1(SessionV1),
    V2(Session),
}

impl Session {
    /// Whether `other` has the same tabs in the same arrangement: at most the carets differ.
    pub fn same_tabs(&self, other: &Self) -> bool {
        self.active == other.active
            && self.pinned == other.pinned
            && self.splits == other.splits
            && self.tabs.len() == other.tabs.len()
            && self
                .tabs
                .iter()
                .zip(&other.tabs)
                .all(|(a, b)| a.note == b.note)
    }

    pub fn encode(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(postcard::to_stdvec(&Stored::V2(self.clone())).unwrap_or_default())
    }

    /// A stored session, or an empty one if it cannot be read. Losing tab state is harmless.
    pub fn decode(bytes: &[u8]) -> Self {
        match postcard::from_bytes::<Stored>(bytes) {
            Ok(Stored::V1(old)) => Self {
                tabs: old.tabs,
                active: old.active,
                ..Self::default()
            },
            Ok(Stored::V2(session)) => session,
            Err(_) => Self::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab() -> OpenTab {
        OpenTab {
            note: NoteId::generate().unwrap(),
            caret_block: 3,
            caret_offset: 7,
        }
    }

    #[test]
    fn a_session_survives_storing() {
        let (a, b) = (tab(), tab());
        let session = Session {
            pinned: vec![a.note],
            splits: vec![Split {
                first: a.note,
                second: b.note,
                stacked: true,
                ratio: 640,
            }],
            tabs: vec![a, b],
            active: Some(1),
        };
        assert_eq!(Session::decode(&session.encode()), session);
    }

    #[test]
    fn a_session_stored_by_the_previous_version_still_opens() {
        let open = tab();
        let bytes = postcard::to_stdvec(&Stored::V1(SessionV1 {
            tabs: vec![open.clone()],
            active: Some(0),
        }))
        .unwrap();
        // V1 is stored as variant 0, as it always was.
        assert_eq!(bytes[0], 0);
        let session = Session::decode(&bytes);
        assert_eq!(session.tabs, [open]);
        assert_eq!(session.active, Some(0));
        assert!(session.pinned.is_empty() && session.splits.is_empty());
    }

    #[test]
    fn unreadable_bytes_give_an_empty_session() {
        assert_eq!(Session::decode(&[255, 1, 2]), Session::default());
    }
}
