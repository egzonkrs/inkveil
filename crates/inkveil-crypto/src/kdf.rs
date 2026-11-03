//! Password-based key derivation with Argon2id.

use std::time::{Duration, Instant};

use argon2::{Algorithm, Argon2, Params, Version};
use serde::{Deserialize, Serialize};

use crate::{CryptoError, KEY_LEN, Password, SecretKey, random::fill_random};

/// Salt length in bytes.
pub const SALT_LEN: usize = 16;

/// A random per-vault salt for password hashing.
pub type Salt = [u8; SALT_LEN];

/// Argon2id cost parameters, stored next to each password key slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory cost in KiB.
    pub memory_kib: u32,
    /// Number of passes over memory.
    pub iterations: u32,
    /// Degree of parallelism (lanes).
    pub parallelism: u32,
}

impl KdfParams {
    /// Parameters for new vaults before calibration: 256 MiB, 3 passes, 4 lanes.
    ///
    /// This is well above the OWASP minimum and takes roughly a second on a current desktop.
    pub const DEFAULT: Self = Self {
        memory_kib: 256 * 1024,
        iterations: 3,
        parallelism: 4,
    };

    /// Cheapest parameters this crate accepts. Only meant for tests.
    pub const TESTING: Self = Self {
        memory_kib: 8 * 1024,
        iterations: 1,
        parallelism: 1,
    };

    // Caps how long a tampered header can make unlocking take (seconds, not hours).
    const MAX_MEMORY_KIB: u32 = 1024 * 1024;
    const MAX_ITERATIONS: u32 = 16;
    const MAX_PARALLELISM: u32 = 16;

    /// Checks the parameters are in a range that is both meaningful and safe to run.
    ///
    /// The upper bounds stop a tampered vault header from making unlock hang or exhaust memory.
    pub fn validate(&self) -> Result<(), CryptoError> {
        if self.memory_kib < Self::TESTING.memory_kib || self.memory_kib > Self::MAX_MEMORY_KIB {
            return Err(CryptoError::InvalidKdfParams("memory out of range"));
        }
        if self.iterations == 0 || self.iterations > Self::MAX_ITERATIONS {
            return Err(CryptoError::InvalidKdfParams("iterations out of range"));
        }
        if self.parallelism == 0 || self.parallelism > Self::MAX_PARALLELISM {
            return Err(CryptoError::InvalidKdfParams("parallelism out of range"));
        }
        Ok(())
    }

    /// Picks the number of passes so that one derivation takes about `target` on this machine.
    ///
    /// Memory and parallelism stay at [`Self::DEFAULT`]. The result never goes below the default
    /// number of passes, so slow machines stay at least as strong as the default.
    pub fn calibrate(target: Duration) -> Result<Self, CryptoError> {
        let probe = Self {
            iterations: 1,
            ..Self::DEFAULT
        };
        let start = Instant::now();
        derive_password_key(&Password::new("calibration"), &[0; SALT_LEN], &probe)?;
        let per_pass = start.elapsed().max(Duration::from_millis(1));

        let passes = (target.as_secs_f64() / per_pass.as_secs_f64()).ceil();
        // Clamped to a small positive range first, so the cast cannot truncate or wrap.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let passes = passes.clamp(1.0, f64::from(Self::MAX_ITERATIONS)) as u32;
        Ok(Self {
            iterations: passes.max(Self::DEFAULT.iterations),
            ..Self::DEFAULT
        })
    }
}

/// Derives a 256-bit key from a password with Argon2id.
pub fn derive_password_key(
    password: &Password,
    salt: &Salt,
    params: &KdfParams,
) -> Result<SecretKey, CryptoError> {
    params.validate()?;
    let argon_params = Params::new(
        params.memory_kib,
        params.iterations,
        params.parallelism,
        Some(KEY_LEN),
    )
    .map_err(|_| CryptoError::InvalidKdfParams("rejected by argon2"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params);

    let mut out = [0u8; KEY_LEN];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|_| CryptoError::InvalidKdfParams("rejected by argon2"))?;
    let key = SecretKey::from_bytes(out);
    zeroize::Zeroize::zeroize(&mut out);
    Ok(key)
}

/// Generates a fresh random salt for password hashing or key wrapping.
pub fn random_salt() -> Result<Salt, CryptoError> {
    let mut salt = [0u8; SALT_LEN];
    fill_random(&mut salt)?;
    Ok(salt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_inputs_same_key() {
        let pw = Password::new("correct horse 1");
        let salt = [1; SALT_LEN];
        let a = derive_password_key(&pw, &salt, &KdfParams::TESTING).unwrap();
        let b = derive_password_key(&pw, &salt, &KdfParams::TESTING).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn salt_and_password_change_the_key() {
        let pw = Password::new("correct horse 1");
        let base = derive_password_key(&pw, &[1; SALT_LEN], &KdfParams::TESTING).unwrap();
        let other_salt = derive_password_key(&pw, &[2; SALT_LEN], &KdfParams::TESTING).unwrap();
        let other_pw = derive_password_key(
            &Password::new("correct horse 2"),
            &[1; SALT_LEN],
            &KdfParams::TESTING,
        )
        .unwrap();
        assert_ne!(base, other_salt);
        assert_ne!(base, other_pw);
    }

    #[test]
    fn hostile_params_are_rejected() {
        let huge = KdfParams {
            memory_kib: u32::MAX,
            ..KdfParams::DEFAULT
        };
        assert!(huge.validate().is_err());
        let zero = KdfParams {
            iterations: 0,
            ..KdfParams::DEFAULT
        };
        assert!(zero.validate().is_err());
        assert!(KdfParams::DEFAULT.validate().is_ok());
    }

    #[test]
    fn random_salts_differ() {
        assert_ne!(random_salt().unwrap(), random_salt().unwrap());
    }
}
