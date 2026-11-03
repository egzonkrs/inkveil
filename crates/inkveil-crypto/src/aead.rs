//! Authenticated encryption with XChaCha20-Poly1305.
//!
//! Sealed layout: `version (1) || nonce (24) || ciphertext || tag (16)`.
//!
//! The 192-bit nonce is random for every message, which is safe for any realistic number of
//! messages under one key. The version byte is authenticated together with the caller's
//! associated data, so it cannot be changed without detection.

use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use zeroize::Zeroizing;

use crate::{CryptoError, SecretKey, random::fill_random};

const FORMAT_VERSION: u8 = 1;

/// Nonce length in bytes.
pub const NONCE_LEN: usize = 24;
/// Authentication tag length in bytes.
pub const TAG_LEN: usize = 16;
/// Bytes that [`seal`] adds to the plaintext length.
pub const SEAL_OVERHEAD: usize = 1 + NONCE_LEN + TAG_LEN;

/// Encrypts and authenticates `plaintext`, binding it to `aad`.
///
/// `aad` is not stored. The same bytes must be passed to [`open`], so use it to bind ciphertext
/// to its place (record type and id), which stops an attacker from swapping records around.
pub fn seal(key: &SecretKey, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce)?;

    let cipher = cipher(key);
    let full_aad = full_aad(aad);
    let ciphertext = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: plaintext,
                aad: &full_aad,
            },
        )
        .map_err(|_| CryptoError::InvalidFormat)?;

    let mut sealed = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    sealed.push(FORMAT_VERSION);
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
}

/// Decrypts data produced by [`seal`] with the same key and `aad`.
///
/// The plaintext is returned in a buffer that is wiped when dropped.
pub fn open(key: &SecretKey, sealed: &[u8], aad: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if sealed.len() < SEAL_OVERHEAD {
        return Err(CryptoError::InvalidFormat);
    }
    let (&version, rest) = sealed.split_first().ok_or(CryptoError::InvalidFormat)?;
    if version != FORMAT_VERSION {
        return Err(CryptoError::InvalidFormat);
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::InvalidFormat)?;

    let full_aad = full_aad(aad);
    cipher(key)
        .decrypt(
            &XNonce::from(nonce),
            Payload {
                msg: ciphertext,
                aad: &full_aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| CryptoError::Decrypt)
}

fn cipher(key: &SecretKey) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(&(*key.as_bytes()).into())
}

fn full_aad(aad: &[u8]) -> Vec<u8> {
    let mut full = Vec::with_capacity(1 + aad.len());
    full.push(FORMAT_VERSION);
    full.extend_from_slice(aad);
    full
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn key() -> SecretKey {
        SecretKey::from_bytes([42; 32])
    }

    #[test]
    fn roundtrip() {
        let sealed = seal(&key(), b"hello", b"note:1").unwrap();
        assert_eq!(sealed.len(), 5 + SEAL_OVERHEAD);
        assert_eq!(
            open(&key(), &sealed, b"note:1").unwrap().as_slice(),
            b"hello"
        );
    }

    #[test]
    fn same_plaintext_gives_different_ciphertext() {
        let a = seal(&key(), b"same", b"").unwrap();
        let b = seal(&key(), b"same", b"").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn wrong_aad_is_rejected() {
        let sealed = seal(&key(), b"hello", b"note:1").unwrap();
        assert_eq!(open(&key(), &sealed, b"note:2"), Err(CryptoError::Decrypt));
    }

    #[test]
    fn every_flipped_bit_is_rejected() {
        let sealed = seal(&key(), b"tamper me", b"ctx").unwrap();
        for byte in 0..sealed.len() {
            for bit in 0..8 {
                let mut damaged = sealed.clone();
                damaged[byte] ^= 1 << bit;
                assert!(
                    open(&key(), &damaged, b"ctx").is_err(),
                    "byte {byte} bit {bit}"
                );
            }
        }
    }

    #[test]
    fn truncated_input_is_rejected() {
        let sealed = seal(&key(), b"", b"").unwrap();
        assert_eq!(
            open(&key(), &sealed[..sealed.len() - 1], b""),
            Err(CryptoError::InvalidFormat)
        );
        assert_eq!(open(&key(), &[], b""), Err(CryptoError::InvalidFormat));
    }

    proptest! {
        #[test]
        fn roundtrip_any(data in proptest::collection::vec(any::<u8>(), 0..4096),
                         aad in proptest::collection::vec(any::<u8>(), 0..64)) {
            let sealed = seal(&key(), &data, &aad).unwrap();
            let opened = open(&key(), &sealed, &aad).unwrap();
            prop_assert_eq!(opened.as_slice(), data.as_slice());
        }
    }
}
