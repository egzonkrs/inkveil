//! Slows down guessing at the unlock screen.
//!
//! Every unlock already costs about a second of Argon2id work. On top of that, after many wrong
//! passwords or recovery keys in a row the vault refuses further attempts for a growing wait. The
//! count is kept in a small file next to the vault, so closing and reopening the app does not
//! reset it, and a correct unlock clears it.
//!
//! This protects the app's own unlock screen. Someone who copies the vault files can try
//! passwords offline without this limit; there, the cost of Argon2id (memory and time per guess)
//! and the strength of the password are what protect the notes.

use std::{fs, path::PathBuf, time::Duration};

use crate::{VaultPaths, fsutil};

/// Wrong attempts allowed before the first wait. Generous, so mistyping never gets in the way.
pub const FREE_ATTEMPTS: u32 = 15;

/// Waits after the free attempts: the 16th wrong attempt in a row waits 30 seconds, the 17th one
/// minute, and so on; from the 21st on, every attempt waits 15 minutes (about 100 guesses a day).
const WAITS: [Duration; 6] = [
    Duration::from_secs(30),
    Duration::from_mins(1),
    Duration::from_mins(2),
    Duration::from_mins(5),
    Duration::from_mins(10),
    Duration::from_mins(15),
];

/// The wait required after `failures` wrong attempts in a row.
pub fn wait_after(failures: u32) -> Duration {
    match failures.checked_sub(FREE_ATTEMPTS + 1) {
        None => Duration::ZERO,
        Some(step) => WAITS[(step as usize).min(WAITS.len() - 1)],
    }
}

/// Wrong unlock attempts in a row, as stored next to the vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attempts {
    pub failures: u32,
    /// Wall-clock time of the last wrong attempt, in milliseconds since the Unix epoch.
    pub last_failure_ms: i64,
}

impl Attempts {
    /// How long until the next attempt is allowed at wall-clock time `now_ms`, if at all.
    ///
    /// A clock set back to before the last failure does not shorten the wait: the full wait
    /// counts from now.
    pub fn wait(&self, now_ms: i64) -> Option<Duration> {
        let wait = wait_after(self.failures);
        if wait.is_zero() {
            return None;
        }
        let elapsed = u64::try_from(now_ms - self.last_failure_ms)
            .map_or(Duration::ZERO, Duration::from_millis);
        let left = wait.saturating_sub(elapsed);
        (!left.is_zero()).then_some(left)
    }

    fn encode(self) -> String {
        format!("v1 {} {}\n", self.failures, self.last_failure_ms)
    }

    fn decode(text: &str) -> Option<Self> {
        let mut parts = text.split_whitespace();
        if parts.next()? != "v1" {
            return None;
        }
        Some(Self {
            failures: parts.next()?.parse().ok()?,
            last_failure_ms: parts.next()?.parse().ok()?,
        })
    }
}

fn path(paths: &VaultPaths) -> PathBuf {
    paths.dir().join("unlock-attempts")
}

/// Reads the stored count. A damaged file counts as having used up the free attempts, so
/// tampering with it never grants more tries than a missing file would.
pub(crate) fn load(paths: &VaultPaths) -> Attempts {
    match fs::read_to_string(path(paths)) {
        Ok(text) => Attempts::decode(&text).unwrap_or(Attempts {
            failures: FREE_ATTEMPTS,
            last_failure_ms: 0,
        }),
        Err(_) => Attempts::default(),
    }
}

pub(crate) fn record_failure(paths: &VaultPaths, now_ms: i64) -> Attempts {
    let previous = load(paths);
    let attempts = Attempts {
        failures: previous.failures.saturating_add(1),
        last_failure_ms: now_ms.max(previous.last_failure_ms),
    };
    if let Err(error) = fsutil::atomic_write(&path(paths), attempts.encode().as_bytes()) {
        tracing::warn!("could not record a failed unlock: {error}");
    }
    attempts
}

pub(crate) fn clear(paths: &VaultPaths) {
    let file = path(paths);
    if file.exists()
        && let Err(error) = fs::remove_file(&file)
    {
        tracing::warn!("could not reset the failed unlock count: {error}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn waits_grow_after_the_free_attempts() {
        assert_eq!(wait_after(0), Duration::ZERO);
        assert_eq!(wait_after(FREE_ATTEMPTS), Duration::ZERO);
        assert_eq!(wait_after(FREE_ATTEMPTS + 1), Duration::from_secs(30));
        assert_eq!(wait_after(FREE_ATTEMPTS + 2), Duration::from_mins(1));
        assert_eq!(wait_after(100), Duration::from_mins(15));
    }

    #[test]
    fn wait_counts_down_and_ignores_a_clock_set_back() {
        let attempts = Attempts {
            failures: FREE_ATTEMPTS + 1,
            last_failure_ms: 1_000_000,
        };
        assert_eq!(attempts.wait(1_000_000), Some(Duration::from_secs(30)));
        assert_eq!(attempts.wait(1_010_000), Some(Duration::from_secs(20)));
        assert_eq!(attempts.wait(1_030_000), None);
        assert_eq!(attempts.wait(0), Some(Duration::from_secs(30)));
    }

    #[test]
    fn stored_count_survives_and_damage_does_not_reset_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(dir.path());
        assert_eq!(load(&paths), Attempts::default());
        record_failure(&paths, 5);
        let attempts = record_failure(&paths, 10);
        assert_eq!(load(&paths), attempts);
        assert_eq!(attempts.failures, 2);
        fs::write(path(&paths), "garbage").unwrap();
        assert_eq!(load(&paths).failures, FREE_ATTEMPTS);
        clear(&paths);
        assert_eq!(load(&paths), Attempts::default());
    }
}
