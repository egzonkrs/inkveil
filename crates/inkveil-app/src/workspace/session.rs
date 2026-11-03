//! Open tabs and caret positions, restored after unlocking.

use inkveil_store::NoteId;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

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
}

#[derive(Serialize, Deserialize)]
enum Stored {
    V1(Session),
}

impl Session {
    pub fn encode(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(postcard::to_stdvec(&Stored::V1(self.clone())).unwrap_or_default())
    }

    /// A stored session, or an empty one if it cannot be read. Losing tab state is harmless.
    pub fn decode(bytes: &[u8]) -> Self {
        match postcard::from_bytes::<Stored>(bytes) {
            Ok(Stored::V1(session)) => session,
            Err(_) => Self::default(),
        }
    }
}
