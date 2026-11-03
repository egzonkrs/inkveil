//! Printable recovery keys.
//!
//! A recovery key is 160 random bits written as 32 Crockford base32 characters in groups of four,
//! for example `7K3M-Q9TD-...`. It has far more entropy than any password, so it is stretched with
//! HKDF rather than a slow password hash.

use std::fmt;

use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{CryptoError, Salt, SecretKey, random::fill_random};

/// Recovery key length in bytes (160 bits).
pub const RECOVERY_KEY_LEN: usize = 20;

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const CHARS: usize = RECOVERY_KEY_LEN * 8 / 5;
const GROUP: usize = 4;

/// Why a typed recovery key could not be read.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryKeyParseError {
    /// The key does not have exactly 32 characters (ignoring dashes and spaces).
    #[error("A recovery key has {expected} characters; this one has {actual}.")]
    WrongLength {
        /// Characters a recovery key has.
        expected: usize,
        /// Characters found.
        actual: usize,
    },
    /// A character that is not part of the recovery key alphabet.
    #[error("'{0}' cannot appear in a recovery key.")]
    InvalidCharacter(char),
}

/// A high-entropy key the user writes down to regain access if every other unlock method fails.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct RecoveryKey([u8; RECOVERY_KEY_LEN]);

impl RecoveryKey {
    /// Generates a new random recovery key.
    pub fn generate() -> Result<Self, CryptoError> {
        let mut key = Self([0; RECOVERY_KEY_LEN]);
        fill_random(&mut key.0)?;
        Ok(key)
    }

    /// Parses a key as the user typed it.
    ///
    /// Case, spaces and dashes are ignored, and the letters `I`, `L` and `O` are read as `1`, `1`
    /// and `0`, so common misreadings of a printed key still work.
    pub fn parse(input: &str) -> Result<Self, RecoveryKeyParseError> {
        let mut values = Zeroizing::new(Vec::with_capacity(CHARS));
        for c in input.chars().filter(|c| !matches!(c, '-' | ' ' | '\t')) {
            values.push(decode_char(c).ok_or(RecoveryKeyParseError::InvalidCharacter(c))?);
        }
        if values.len() != CHARS {
            return Err(RecoveryKeyParseError::WrongLength {
                expected: CHARS,
                actual: values.len(),
            });
        }

        let mut key = Self([0; RECOVERY_KEY_LEN]);
        let mut acc: u64 = 0;
        let mut bits = 0;
        let mut out = 0;
        for &v in values.iter() {
            acc = (acc << 5) | u64::from(v);
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                key.0[out] = (acc >> bits).to_le_bytes()[0];
                out += 1;
            }
        }
        acc.zeroize();
        Ok(key)
    }

    /// The key formatted for display and printing, e.g. `7K3M-Q9TD-…` (39 characters).
    pub fn display(&self) -> Zeroizing<String> {
        let mut text = Zeroizing::new(String::with_capacity(CHARS + CHARS / GROUP));
        let mut acc: u64 = 0;
        let mut bits = 0;
        let mut written = 0;
        for &byte in &self.0 {
            acc = (acc << 8) | u64::from(byte);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                if written > 0 && written % GROUP == 0 {
                    text.push('-');
                }
                text.push(char::from(ALPHABET[((acc >> bits) & 0x1F) as usize]));
                written += 1;
            }
        }
        acc.zeroize();
        text
    }

    /// Derives the key that wraps the vault key in a recovery slot.
    pub fn derive_wrapping_key(&self, salt: &Salt) -> SecretKey {
        SecretKey::derive_from(&self.0, Some(salt), b"inkveil/v1/recovery-slot")
    }
}

impl fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryKey(<redacted>)")
    }
}

fn decode_char(c: char) -> Option<u8> {
    let c = match c.to_ascii_uppercase() {
        'I' | 'L' => '1',
        'O' => '0',
        other => other,
    };
    ALPHABET
        .iter()
        .position(|&a| char::from(a) == c)
        .and_then(|p| u8::try_from(p).ok())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn display_shape() {
        let key = RecoveryKey::generate().unwrap();
        let shown = key.display();
        assert_eq!(shown.len(), 39);
        assert_eq!(shown.split('-').count(), 8);
        assert!(shown.split('-').all(|g| g.len() == 4));
    }

    #[test]
    fn forgiving_parse() {
        let key = RecoveryKey([0; RECOVERY_KEY_LEN]);
        assert_eq!(
            key.display().as_str(),
            "0000-0000-0000-0000-0000-0000-0000-0000"
        );
        let typed = "oooo oooo-0000 0000 0000 0000 0000 000o";
        assert_eq!(RecoveryKey::parse(typed).unwrap().0, key.0);
    }

    #[test]
    fn bad_input() {
        assert_eq!(
            RecoveryKey::parse("ABCD").err(),
            Some(RecoveryKeyParseError::WrongLength {
                expected: 32,
                actual: 4
            })
        );
        assert_eq!(
            RecoveryKey::parse("U000-0000-0000-0000-0000-0000-0000-0000").err(),
            Some(RecoveryKeyParseError::InvalidCharacter('U'))
        );
    }

    #[test]
    fn wrapping_key_depends_on_salt() {
        let key = RecoveryKey::generate().unwrap();
        assert_ne!(
            key.derive_wrapping_key(&[1; 16]),
            key.derive_wrapping_key(&[2; 16])
        );
    }

    proptest! {
        #[test]
        fn display_parse_roundtrip(bytes in any::<[u8; RECOVERY_KEY_LEN]>()) {
            let key = RecoveryKey(bytes);
            let parsed = RecoveryKey::parse(&key.display()).unwrap();
            prop_assert_eq!(parsed.0, bytes);
            let lower = key.display().to_lowercase();
            prop_assert_eq!(RecoveryKey::parse(&lower).unwrap().0, bytes);
        }
    }
}
