use thiserror::Error;

/// Errors from cryptographic operations.
///
/// Decryption failures are deliberately vague: callers learn only that the data could not be
/// authenticated, never why.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CryptoError {
    /// The operating system could not provide random bytes.
    #[error("the operating system random number generator failed")]
    Random,
    /// Data failed authentication: wrong key, wrong context, or the bytes were altered.
    #[error("data could not be decrypted (wrong key or damaged data)")]
    Decrypt,
    /// Sealed data is too short or has an unknown format version.
    #[error("encrypted data has an invalid format")]
    InvalidFormat,
    /// Password hashing parameters are outside the supported range.
    #[error("invalid key derivation parameters: {0}")]
    InvalidKdfParams(&'static str),
}
