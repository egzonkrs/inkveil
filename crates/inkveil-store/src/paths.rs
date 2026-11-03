use std::path::{Path, PathBuf};

/// Locations of the files that make up one vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultPaths {
    dir: PathBuf,
}

impl VaultPaths {
    /// Uses `dir` as the vault folder. Nothing is created until a vault is created or opened.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The vault folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The SQLite database.
    pub fn database(&self) -> PathBuf {
        self.dir.join("vault.db")
    }

    /// The database while a new vault is being created, before it is moved into place.
    pub(crate) fn database_in_progress(&self) -> PathBuf {
        self.dir.join("vault.db.creating")
    }

    /// The copy of the key header kept outside the database.
    pub fn header(&self) -> PathBuf {
        self.dir.join("vault.header")
    }

    /// The file locked while the vault is open.
    pub fn lock(&self) -> PathBuf {
        self.dir.join("vault.lock")
    }

    /// The folder for automatic backups.
    pub fn backups(&self) -> PathBuf {
        self.dir.join("backups")
    }

    /// Whether a vault database exists in this folder.
    pub fn exists(&self) -> bool {
        self.database().is_file()
    }

    /// Whether the folder holds a vault's key header or backups. Without the database, the
    /// database went missing (moved, quarantined, a sync or cleanup tool): a new vault must not
    /// be created here, since it would replace the header and its backups would push out the old
    /// vault's.
    pub fn has_vault_files(&self) -> bool {
        self.header().is_file()
            || crate::maintenance::list_backups(self).is_ok_and(|backups| !backups.is_empty())
    }
}
