use std::fmt;

use thiserror::Error;
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

/// Minimum password length, counted in Unicode characters.
pub const MIN_PASSWORD_CHARS: usize = 8;

/// Why a new password was refused.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum PasswordPolicyError {
    /// Fewer than [`MIN_PASSWORD_CHARS`] characters.
    #[error("Password must be at least {min} characters (it has {actual}).")]
    TooShort {
        /// Required minimum.
        min: usize,
        /// Characters in the given password.
        actual: usize,
    },
    /// No digit anywhere in the password.
    #[error("Password must contain at least one number.")]
    MissingNumber,
}

/// A password, normalized to Unicode NFC and wiped from memory on drop.
///
/// Normalizing means the same password typed on different keyboards or systems (which may
/// produce composed or decomposed accents) always derives the same key.
pub struct Password(Zeroizing<String>);

impl Password {
    /// Wraps a password as typed. Does not check the policy; use [`Password::check_policy`] when
    /// a password is being chosen, not when it is entered to unlock.
    pub fn new(raw: &str) -> Self {
        Self(Zeroizing::new(raw.nfc().collect()))
    }

    /// Checks the rules for a new password: at least 8 characters and at least one number.
    pub fn check_policy(&self) -> Result<(), PasswordPolicyError> {
        let actual = self.0.chars().count();
        if actual < MIN_PASSWORD_CHARS {
            return Err(PasswordPolicyError::TooShort {
                min: MIN_PASSWORD_CHARS,
                actual,
            });
        }
        if !self.0.chars().any(char::is_numeric) {
            return Err(PasswordPolicyError::MissingNumber);
        }
        Ok(())
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy() {
        assert_eq!(
            Password::new("abc1").check_policy(),
            Err(PasswordPolicyError::TooShort { min: 8, actual: 4 })
        );
        assert_eq!(
            Password::new("abcdefgh").check_policy(),
            Err(PasswordPolicyError::MissingNumber)
        );
        assert_eq!(Password::new("abcdefg1").check_policy(), Ok(()));
        // No other rules: no uppercase or symbol required.
        assert_eq!(Password::new("        1").check_policy(), Ok(()));
    }

    #[test]
    fn length_counts_characters_not_bytes() {
        // Seven two-byte characters plus a digit is eight characters.
        assert_eq!(Password::new("ëëëëëëë1").check_policy(), Ok(()));
    }

    #[test]
    fn composed_and_decomposed_forms_are_equal() {
        let composed = Password::new("caf\u{e9}12345");
        let decomposed = Password::new("cafe\u{301}12345");
        assert_eq!(composed.as_bytes(), decomposed.as_bytes());
    }

    #[test]
    fn debug_is_redacted() {
        assert_eq!(
            format!("{:?}", Password::new("secret123")),
            "Password(<redacted>)"
        );
    }
}
