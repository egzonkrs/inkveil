//! Daily backups also copied to a folder the user picks: another drive, a USB stick or a synced
//! folder, so the notes survive if this PC or its disk fails. The copies are the vault's own
//! encrypted backups; nothing is decrypted to make them.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gpui::{Context, PathPromptOptions, SharedString, Window};
use inkveil_store::StoreError;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::Workspace;
use crate::storage::state_keys;

/// Folder created inside the one the user picks, so backups never mix with their own files.
const SUBFOLDER: &str = "Inkveil backups";

/// How often the app checks, while unlocked, whether a daily backup is due.
const CHECK_EVERY: Duration = Duration::from_hours(1);

/// What the settings menu shows about backups.
pub struct BackupState {
    /// Where backups are also copied, besides the vault's own `backups` folder.
    pub folder: Option<PathBuf>,
    /// When the newest backup was made, from the last backup round.
    pub newest_ms: Option<i64>,
    /// Why the last copy to the folder failed, until one succeeds.
    pub copy_error: Option<SharedString>,
    /// A backup asked for in the menu is being made.
    pub running: bool,
    last_check: Instant,
}

impl Default for BackupState {
    fn default() -> Self {
        Self {
            folder: None,
            newest_ms: None,
            copy_error: None,
            running: false,
            last_check: Instant::now(),
        }
    }
}

/// The stored form; new fields mean a new variant.
#[derive(Serialize, Deserialize)]
enum Stored {
    V1 { folder: Option<String> },
}

fn encode(folder: Option<&Path>) -> Zeroizing<Vec<u8>> {
    let stored = Stored::V1 {
        folder: folder.and_then(Path::to_str).map(str::to_owned),
    };
    Zeroizing::new(postcard::to_stdvec(&stored).unwrap_or_default())
}

fn decode(bytes: &[u8]) -> Option<PathBuf> {
    match postcard::from_bytes::<Stored>(bytes) {
        Ok(Stored::V1 { folder }) => folder.map(PathBuf::from),
        Err(_) => None,
    }
}

/// The folder backups go to when the user picks `chosen`.
fn backup_folder_in(chosen: &Path) -> PathBuf {
    if chosen.file_name().is_some_and(|name| name == SUBFOLDER) {
        chosen.to_path_buf()
    } else {
        chosen.join(SUBFOLDER)
    }
}

/// A path short enough for the menu: `D:\…\Backups\Inkveil backups`.
pub fn short_path(path: &Path) -> String {
    let parts: Vec<_> = path.components().collect();
    if parts.len() <= 4 {
        return path.display().to_string();
    }
    let tail: PathBuf = parts[parts.len() - 2..].iter().collect();
    format!(
        "{}…\\{}",
        parts[0].as_os_str().to_string_lossy(),
        tail.display()
    )
}

impl Workspace {
    /// Loads the backup folder after unlocking. Runs before the first backup round, so that round
    /// already copies to it.
    pub(super) fn load_backup_folder(&mut self, bytes: Option<&[u8]>) {
        self.backups.folder = bytes.and_then(decode);
        self.storage.set_backup_folder(self.backups.folder.clone());
    }

    /// Called every tick: checks hourly whether a daily backup is due, so one is made each day
    /// even when the vault stays unlocked for days.
    pub(super) fn backup_tick(&mut self) {
        if self.loaded && self.backups.last_check.elapsed() >= CHECK_EVERY {
            self.backups.last_check = Instant::now();
            self.storage.back_up(false);
        }
    }

    pub(super) fn on_backed_up(&mut self, newest_ms: Option<i64>, copy_error: Option<StoreError>) {
        self.backups.running = false;
        self.backups.newest_ms = newest_ms;
        self.backups.copy_error = copy_error.map(|e| e.to_string().into());
    }

    /// Asks for a folder, then copies the newest backup there right away.
    pub(super) fn choose_backup_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_settings = false;
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Copy backups here".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(chosen) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| this.set_backup_folder(&chosen, cx))
                .ok();
        })
        .detach();
    }

    fn set_backup_folder(&mut self, chosen: &Path, cx: &mut Context<Self>) {
        let folder = backup_folder_in(chosen);
        let problem = if folder.starts_with(crate::vault_dir()) {
            Some("Choose a folder outside the vault folder, ideally on another drive.")
        } else if folder.to_str().is_none() {
            Some("That folder's name cannot be saved. Choose another folder.")
        } else {
            None
        };
        if let Some(problem) = problem {
            self.backups.copy_error = Some(problem.into());
            self.show_settings = true;
            cx.notify();
            return;
        }
        self.storage
            .put_state(state_keys::BACKUP_FOLDER, encode(Some(&folder)));
        self.storage.set_backup_folder(Some(folder.clone()));
        self.backups.folder = Some(folder);
        self.backups.copy_error = None;
        self.backups.running = true;
        self.storage.back_up(false);
        self.show_settings = true;
        cx.notify();
    }

    /// Stops copying; the copies already in the folder stay there.
    pub(super) fn stop_backup_copies(&mut self, cx: &mut Context<Self>) {
        self.storage
            .put_state(state_keys::BACKUP_FOLDER, encode(None));
        self.storage.set_backup_folder(None);
        self.backups.folder = None;
        self.backups.copy_error = None;
        cx.notify();
    }

    /// Makes a backup now and copies it to the folder.
    pub(super) fn back_up_now(&mut self, cx: &mut Context<Self>) {
        self.backups.running = true;
        self.storage.back_up(true);
        cx.notify();
    }

    pub(super) fn open_backup_folder(&self, cx: &mut Context<Self>) {
        if let Some(folder) = &self.backups.folder
            && folder.is_dir()
        {
            cx.open_with_system(folder);
        }
    }

    /// One line for the settings menu, and whether it reports a problem.
    pub(super) fn backup_status(&self) -> (String, bool) {
        let backups = &self.backups;
        if backups.running {
            return ("Backing up…".into(), false);
        }
        if let Some(error) = &backups.copy_error {
            return (format!("Could not copy the backup: {error}"), true);
        }
        let when = backups
            .newest_ms
            .map(|ms| super::sidebar::relative_time(ms, inkveil_store::now_ms()));
        let text = match (when, &backups.folder) {
            (Some(when), Some(_)) => format!("Last backup: {when}. Also copied to the folder."),
            (Some(when), None) => format!("Last backup: {when}. Kept next to the vault only."),
            (None, _) => "A backup is made each day you use Inkveil.".into(),
        };
        (text, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_setting_round_trips() {
        let folder = PathBuf::from(r"D:\Backups\Inkveil backups");
        assert_eq!(decode(&encode(Some(&folder))), Some(folder));
        assert_eq!(decode(&encode(None)), None);
        assert_eq!(decode(b"garbage"), None);
    }

    #[test]
    fn backups_go_in_their_own_subfolder() {
        assert_eq!(
            backup_folder_in(Path::new(r"D:\Backups")),
            PathBuf::from(r"D:\Backups\Inkveil backups")
        );
        assert_eq!(
            backup_folder_in(Path::new(r"D:\Backups\Inkveil backups")),
            PathBuf::from(r"D:\Backups\Inkveil backups")
        );
    }
}
