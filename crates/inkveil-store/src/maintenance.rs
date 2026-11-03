//! Backups and history thinning: which copies to keep.

use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use crate::{RevisionId, StoreError, VaultPaths, now_ms};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// The newest backups kept no matter how close together.
const RECENT_BACKUPS: usize = 7;
/// Also kept: the newest backup of each of this many most recent days that have one, so several
/// backups made in one day never push out the days before.
const DAILY_BACKUPS: usize = 7;
/// Older backups kept, each at least a week apart.
const WEEKLY_BACKUPS: usize = 4;
/// The newest revisions of a note kept no matter how close together.
const RECENT_REVISIONS: usize = 20;
/// Older revisions are kept one per day for this many days.
const DAILY_REVISION_DAYS: i64 = 30;

/// Backups in the vault's backup folder, newest first, with their creation time.
///
/// Backups are named `vault-<unix ms>.db`; anything else in the folder is ignored.
pub fn list_backups(paths: &VaultPaths) -> Result<Vec<(PathBuf, i64)>, StoreError> {
    backups_in(&paths.backups())
}

/// Backups in `dir` (named `vault-<unix ms>.db`), newest first, with their creation time.
pub fn backups_in(dir: &Path) -> Result<Vec<(PathBuf, i64)>, StoreError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut backups: Vec<(PathBuf, i64)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let ms = name
                .strip_prefix("vault-")?
                .strip_suffix(".db")?
                .parse()
                .ok()?;
            Some((entry.path(), ms))
        })
        .collect();
    backups.sort_by_key(|b| std::cmp::Reverse(b.1));
    Ok(backups)
}

/// Writes a new backup with `write` (given a temporary path), makes it durable, gives it its
/// final name and deletes backups no longer needed. Returns the backup's path.
pub(crate) fn write_backup(
    paths: &VaultPaths,
    write: impl FnOnce(&Path) -> Result<(), StoreError>,
) -> Result<PathBuf, StoreError> {
    let dir = paths.backups();
    fs::create_dir_all(&dir)?;
    // Leftovers of a backup interrupted by a crash.
    for entry in fs::read_dir(&dir)?.filter_map(Result::ok) {
        if entry.path().extension().is_some_and(|e| e == "tmp") {
            let _ = fs::remove_file(entry.path());
        }
    }
    let at = now_ms();
    let tmp = dir.join(format!("vault-{at}.db.tmp"));
    let path = dir.join(format!("vault-{at}.db"));
    write(&tmp)?;
    // Windows can only flush a handle opened for writing.
    fs::OpenOptions::new().write(true).open(&tmp)?.sync_all()?;
    fs::rename(&tmp, &path)?;

    let existing = list_backups(paths)?;
    for doomed in backups_to_delete(&existing) {
        fs::remove_file(doomed)?;
    }
    Ok(path)
}

/// Copies the vault's newest backup into `folder` (another drive, a synced folder) unless it is
/// there already, then deletes copies there that [`backups_to_delete`] would not keep. The copy
/// is written under a temporary name, flushed and checked against the original before it gets
/// its final name, so a full disk or a pulled drive never leaves a broken backup behind.
/// Returns the new copy's path, or `None` if there was nothing new to copy.
///
/// Copies stay encrypted exactly like the vault and open with its password or recovery key.
/// Only files named like backups are ever deleted from `folder`.
pub fn copy_backups_to(paths: &VaultPaths, folder: &Path) -> Result<Option<PathBuf>, StoreError> {
    if is_within(folder, paths.dir()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the backup folder must be outside the vault folder",
        )
        .into());
    }
    let Some((newest, _)) = list_backups(paths)?.into_iter().next() else {
        return Ok(None);
    };
    let Some(name) = newest.file_name() else {
        return Ok(None);
    };
    fs::create_dir_all(folder)?;
    // Leftovers of a copy interrupted by the app closing or a drive being pulled.
    for entry in fs::read_dir(folder)?.filter_map(Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("vault-") && name.ends_with(".db.tmp") {
            let _ = fs::remove_file(entry.path());
        }
    }
    let target = folder.join(name);
    let copied = if target.is_file() {
        None
    } else {
        let tmp = target.with_extension("db.tmp");
        let result = (|| {
            fs::copy(&newest, &tmp)?;
            fs::OpenOptions::new().write(true).open(&tmp)?.sync_all()?;
            if !same_contents(&tmp, &newest)? {
                return Err(StoreError::Corrupt(
                    "the backup copy does not match the backup",
                ));
            }
            fs::rename(&tmp, &target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        Some(target)
    };
    for doomed in backups_to_delete(&backups_in(folder)?) {
        fs::remove_file(doomed)?;
    }
    Ok(copied)
}

/// Whether `path` is `dir` or inside it, ignoring letter case as Windows does.
fn is_within(path: &Path, dir: &Path) -> bool {
    let parts = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    parts(path).starts_with(&parts(dir))
}

/// Whether two files hold the same bytes, read in chunks so large vaults need little memory.
fn same_contents(first: &Path, second: &Path) -> std::io::Result<bool> {
    let (mut first, mut second) = (fs::File::open(first)?, fs::File::open(second)?);
    if first.metadata()?.len() != second.metadata()?.len() {
        return Ok(false);
    }
    let (mut ours, mut theirs) = (vec![0; 64 * 1024], vec![0; 64 * 1024]);
    loop {
        let read = first.read(&mut ours)?;
        if read == 0 {
            return Ok(true);
        }
        second.read_exact(&mut theirs[..read])?;
        if ours[..read] != theirs[..read] {
            return Ok(false);
        }
    }
}

/// Deletes every backup (after the vault key changed, old backups open with the old password).
pub(crate) fn delete_backups(paths: &VaultPaths) -> Result<(), StoreError> {
    for (path, _) in list_backups(paths)? {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Which backups to delete: keeps the newest [`RECENT_BACKUPS`] and the newest of each of the
/// last [`DAILY_BACKUPS`] days, then up to [`WEEKLY_BACKUPS`] older ones that are each at least a
/// week older than the previous one kept.
pub fn backups_to_delete(backups_newest_first: &[(PathBuf, i64)]) -> Vec<PathBuf> {
    let mut doomed = Vec::new();
    let mut last_kept: Option<i64> = None;
    let mut weekly = 0;
    let mut days: Vec<i64> = Vec::new();
    for (index, (path, at)) in backups_newest_first.iter().enumerate() {
        let day = at.div_euclid(DAY_MS);
        let new_day = !days.contains(&day);
        if new_day {
            days.push(day);
        }
        let keep = if index < RECENT_BACKUPS || (new_day && days.len() <= DAILY_BACKUPS) {
            true
        } else if weekly < WEEKLY_BACKUPS && last_kept.is_none_or(|k| k - at >= 7 * DAY_MS) {
            weekly += 1;
            true
        } else {
            false
        };
        if keep {
            last_kept = Some(*at);
        } else {
            doomed.push(path.clone());
        }
    }
    doomed
}

/// Which revisions of a note to keep, given `(id, modified time)` pairs in any order:
/// the newest [`RECENT_REVISIONS`]; for older ones, the newest of each day within
/// [`DAILY_REVISION_DAYS`]; and always the very first revision.
pub fn revisions_to_keep(revisions: &[(RevisionId, i64)], now: i64) -> HashSet<RevisionId> {
    let mut sorted = revisions.to_vec();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
    let mut keep: HashSet<RevisionId> = sorted.iter().take(RECENT_REVISIONS).map(|r| r.0).collect();
    // Days already represented by a recent revision need no daily one.
    let mut days_seen: HashSet<i64> = sorted
        .iter()
        .take(RECENT_REVISIONS)
        .map(|(_, at)| at.div_euclid(DAY_MS))
        .collect();
    for (id, at) in sorted.iter().skip(RECENT_REVISIONS) {
        if now - at <= DAILY_REVISION_DAYS * DAY_MS && days_seen.insert(at.div_euclid(DAY_MS)) {
            keep.insert(*id);
        }
    }
    if let Some((first, _)) = sorted.last() {
        keep.insert(*first);
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backup(at: i64) -> (PathBuf, i64) {
        (PathBuf::from(format!("vault-{at}.db")), at)
    }

    #[test]
    fn keeps_recent_and_weekly_backups() {
        // One backup a day for 60 days, newest first.
        let backups: Vec<_> = (0..60).rev().map(|d| backup(d * DAY_MS)).collect();
        let doomed = backups_to_delete(&backups);
        let kept: Vec<i64> = backups
            .iter()
            .filter(|(p, _)| !doomed.contains(p))
            .map(|(_, at)| at / DAY_MS)
            .collect();
        assert_eq!(kept, [59, 58, 57, 56, 55, 54, 53, 46, 39, 32, 25]);
    }

    #[test]
    fn many_backups_in_one_day_keep_the_days_before() {
        // One a day for 10 days, then 20 more today (pressing "Back up now" again and again).
        let mut backups: Vec<_> = (0..10).map(|d| backup(d * DAY_MS)).collect();
        backups.extend((1..=20).map(|i| backup(9 * DAY_MS + i * 1000)));
        backups.reverse();
        let doomed = backups_to_delete(&backups);
        let kept_days: HashSet<i64> = backups
            .iter()
            .filter(|(p, _)| !doomed.contains(p))
            .map(|(_, at)| at / DAY_MS)
            .collect();
        // Today and the six days before are all still there.
        assert!((3..=9).all(|d| kept_days.contains(&d)), "{kept_days:?}");
    }

    #[test]
    fn folders_inside_the_vault_are_recognised() {
        let vault = Path::new(r"C:\Users\Me\AppData\Local\Inkveil\vault");
        assert!(is_within(&vault.join("copies"), vault));
        assert!(is_within(
            Path::new(r"c:\users\me\appdata\local\inkveil\VAULT\x"),
            vault
        ));
        assert!(!is_within(
            Path::new(r"C:\Users\Me\AppData\Local\Inkveil\vault-copies"),
            vault
        ));
    }

    #[test]
    fn few_backups_are_all_kept() {
        let backups: Vec<_> = (0..5).rev().map(backup).collect();
        assert_eq!(backups_to_delete(&backups), Vec::<PathBuf>::new());
    }

    #[test]
    fn revision_thinning() {
        let now = 100 * DAY_MS;
        // Every hour for the last 3 days, plus a very old first revision.
        let mut revisions: Vec<(RevisionId, i64)> = (0..72)
            .map(|h| (RevisionId(h + 10), now - h * 60 * 60 * 1000))
            .collect();
        revisions.push((RevisionId(1), DAY_MS));
        let keep = revisions_to_keep(&revisions, now);
        // 20 newest, one per older day inside the window (3 days, minus days already covered by
        // the newest 20), and the first.
        assert!(keep.contains(&RevisionId(1)));
        assert!((0..20).all(|h| keep.contains(&RevisionId(h + 10))));
        assert!(keep.len() < 30, "kept {}", keep.len());
        assert!(keep.len() >= 22);
    }

    #[test]
    fn small_histories_are_untouched() {
        let revisions: Vec<_> = (0..5).map(|i| (RevisionId(i), i * 1000)).collect();
        assert_eq!(revisions_to_keep(&revisions, 10_000).len(), 5);
    }
}
