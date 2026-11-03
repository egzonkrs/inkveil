use crate::CryptoError;

/// Fills `buf` with cryptographically secure random bytes from the operating system.
pub fn fill_random(buf: &mut [u8]) -> Result<(), CryptoError> {
    getrandom::fill(buf).map_err(|_| CryptoError::Random)
}
