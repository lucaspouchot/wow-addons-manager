//! Exclusive per-profile and config locks.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use crate::error::{AppError, Result};

/// Held for as long as the value lives. The OS releases the lock if the process dies.
#[derive(Debug)]
pub struct FileLock {
    _file: File,
}

impl FileLock {
    /// Take the lock or fail right away with `AppError::Locked(label)`.
    pub fn acquire(path: &Path, label: &str) -> Result<FileLock> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::io("cannot create folder", parent, e))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|e| AppError::io("cannot open lock file", path, e))?;
        match file.try_lock() {
            Ok(()) => Ok(FileLock { _file: file }),
            Err(TryLockError::WouldBlock) => Err(AppError::Locked(label.to_string())),
            Err(TryLockError::Error(e)) => Err(AppError::io("cannot lock", path, e)),
        }
    }
}
