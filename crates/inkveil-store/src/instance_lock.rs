use std::{
    fs::{File, OpenOptions, TryLockError},
    path::Path,
};

use crate::StoreError;

/// An exclusive lock on the vault, released when dropped (or when the process dies).
#[derive(Debug)]
pub(crate) struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// Takes the lock, or fails with [`StoreError::AlreadyOpen`] if another process holds it.
    pub(crate) fn acquire(path: &Path) -> Result<Self, StoreError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(StoreError::AlreadyOpen),
            Err(TryLockError::Error(e)) => Err(e.into()),
        }
    }
}
