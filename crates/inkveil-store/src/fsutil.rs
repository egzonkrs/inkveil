use std::{
    fs::{self, File},
    io::{self, Write},
    path::Path,
};

/// Replaces `path` with `contents` so that readers see either the old or the new file, never a
/// partial one, even if the process or machine dies halfway.
///
/// Writes a temporary file next to the target, flushes it to disk, renames it over the target
/// and, on Unix, flushes the directory so the rename itself is durable.
pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = File::create(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    sync_parent_dir(path)
}

#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(dir) => File::open(dir)?.sync_all(),
        None => Ok(()),
    }
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn sync_parent_dir(_path: &Path) -> io::Result<()> {
    // NTFS journals renames itself; there is no directory handle to flush.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.bin");
        atomic_write(&path, b"one").unwrap();
        atomic_write(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(!path.with_extension("tmp").exists());
    }
}
