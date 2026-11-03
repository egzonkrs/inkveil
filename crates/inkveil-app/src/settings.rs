//! User settings, stored encrypted in the vault, and the auto-lock rules.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::editor::EditorSettings;

/// When the vault locks itself after no keyboard or mouse input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoLock {
    /// After this many minutes without input.
    Minutes(u32),
    /// Only when locked by hand, when the app closes or when the PC restarts or shuts down.
    Never,
}

impl AutoLock {
    /// The choices offered in the settings menu.
    pub const CHOICES: [Self; 7] = [
        Self::Minutes(1),
        Self::Minutes(5),
        Self::Minutes(15),
        Self::Minutes(30),
        Self::Minutes(60),
        Self::Minutes(240),
        Self::Never,
    ];

    pub fn timeout(self) -> Option<Duration> {
        match self {
            Self::Minutes(m) => Some(Duration::from_secs(u64::from(m) * 60)),
            Self::Never => None,
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Minutes(1) => "1 minute".into(),
            Self::Minutes(m) if m % 60 == 0 && m >= 60 => {
                let h = m / 60;
                if h == 1 {
                    "1 hour".into()
                } else {
                    format!("{h} hours")
                }
            }
            Self::Minutes(m) => format!("{m} minutes"),
            Self::Never => "Never".into(),
        }
    }
}

/// Everything the user can configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    pub auto_lock: AutoLock,
    /// Lock when Windows locks (Win+L, lock screen) or the PC sleeps or hibernates.
    pub lock_with_system: bool,
    pub editor: EditorSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            auto_lock: AutoLock::Minutes(15),
            lock_with_system: true,
            editor: EditorSettings::default(),
        }
    }
}

/// Settings as stored before spellcheck could be switched off.
#[derive(Serialize, Deserialize)]
struct SettingsV1 {
    auto_lock: AutoLock,
    lock_with_system: bool,
    wrap: bool,
    line_numbers: bool,
}

/// The stored form. Old versions stay readable; new fields mean a new variant.
#[derive(Serialize, Deserialize)]
enum Stored {
    V1(SettingsV1),
    V2(AppSettings),
}

impl AppSettings {
    pub fn encode(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(postcard::to_stdvec(&Stored::V2(*self)).unwrap_or_default())
    }

    /// Stored settings, or the defaults if they cannot be read.
    pub fn decode(bytes: &[u8]) -> Self {
        match postcard::from_bytes::<Stored>(bytes) {
            Ok(Stored::V2(settings)) => settings,
            Ok(Stored::V1(old)) => Self {
                auto_lock: old.auto_lock,
                lock_with_system: old.lock_with_system,
                editor: EditorSettings {
                    wrap: old.wrap,
                    line_numbers: old.line_numbers,
                    spellcheck: true,
                },
            },
            Err(_) => Self::default(),
        }
    }
}

/// Why the vault locked itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockReason {
    Idle,
    SystemSlept,
    SystemLocked,
}

/// A gap between two ticks longer than this means the PC was asleep (ticks run 4 times a second).
pub const SLEEP_GAP: Duration = Duration::from_secs(20);

/// Decides whether to lock now.
///
/// - `idle`: time since the last keyboard or mouse input, the larger of the monotonic and the
///   wall-clock measurement, so time spent asleep counts even if the monotonic clock paused.
/// - `tick_gap`: wall-clock time since the previous check.
/// - `session_locked`: whether Windows reports the session as locked, if known.
pub fn should_lock(
    settings: &AppSettings,
    idle: Duration,
    tick_gap: Duration,
    session_locked: Option<bool>,
) -> Option<LockReason> {
    if settings.lock_with_system {
        if session_locked == Some(true) {
            return Some(LockReason::SystemLocked);
        }
        if tick_gap >= SLEEP_GAP {
            return Some(LockReason::SystemSlept);
        }
    }
    match settings.auto_lock.timeout() {
        Some(timeout) if idle >= timeout => Some(LockReason::Idle),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: Duration = Duration::from_secs(1);
    const MIN: Duration = Duration::from_secs(60);

    fn with(auto_lock: AutoLock, lock_with_system: bool) -> AppSettings {
        AppSettings {
            auto_lock,
            lock_with_system,
            ..AppSettings::default()
        }
    }

    #[test]
    fn idle_timeouts() {
        let s = with(AutoLock::Minutes(5), false);
        assert_eq!(should_lock(&s, 4 * MIN, SEC, None), None);
        assert_eq!(should_lock(&s, 5 * MIN, SEC, None), Some(LockReason::Idle));
        let hour = with(AutoLock::Minutes(60), false);
        assert_eq!(should_lock(&hour, 59 * MIN, SEC, None), None);
        assert_eq!(
            should_lock(&hour, 61 * MIN, SEC, None),
            Some(LockReason::Idle)
        );
    }

    #[test]
    fn never_only_locks_for_the_system() {
        let s = with(AutoLock::Never, false);
        assert_eq!(should_lock(&s, 1000 * MIN, 600 * SEC, Some(true)), None);
        let s = with(AutoLock::Never, true);
        assert_eq!(should_lock(&s, 1000 * MIN, SEC, Some(false)), None);
        assert_eq!(
            should_lock(&s, Duration::ZERO, SEC, Some(true)),
            Some(LockReason::SystemLocked)
        );
    }

    #[test]
    fn sleep_and_windows_lock() {
        let s = with(AutoLock::Minutes(15), true);
        assert_eq!(
            should_lock(&s, SEC, 30 * SEC, None),
            Some(LockReason::SystemSlept)
        );
        assert_eq!(
            should_lock(&s, SEC, SEC, Some(true)),
            Some(LockReason::SystemLocked)
        );
        assert_eq!(should_lock(&s, SEC, SEC, Some(false)), None);
        let off = with(AutoLock::Minutes(15), false);
        assert_eq!(should_lock(&off, SEC, 30 * SEC, Some(true)), None);
    }

    #[test]
    fn labels_and_roundtrip() {
        let labels: Vec<String> = AutoLock::CHOICES.iter().map(|c| c.label()).collect();
        assert_eq!(
            labels,
            [
                "1 minute",
                "5 minutes",
                "15 minutes",
                "30 minutes",
                "1 hour",
                "4 hours",
                "Never"
            ]
        );
        let settings = with(AutoLock::Never, false);
        assert_eq!(AppSettings::decode(&settings.encode()), settings);
        assert_eq!(AppSettings::decode(b"junk"), AppSettings::default());
    }

    #[test]
    fn settings_saved_before_the_spellcheck_option_still_load() {
        let old = postcard::to_stdvec(&Stored::V1(SettingsV1 {
            auto_lock: AutoLock::Minutes(60),
            lock_with_system: false,
            wrap: false,
            line_numbers: true,
        }))
        .unwrap_or_default();
        let settings = AppSettings::decode(&old);
        assert_eq!(settings.auto_lock, AutoLock::Minutes(60));
        assert!(!settings.lock_with_system && !settings.editor.wrap);
        assert!(settings.editor.spellcheck);
    }
}
