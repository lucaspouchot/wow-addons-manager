//! Small filesystem helpers: atomic writes, renames with retry, folder copies.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use crate::error::{AppError, Result};

/// Write a file atomically: temp file in the same folder, fsync, rename.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| AppError::io("cannot create folder", parent, e))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".tmp-")
        .tempfile_in(parent)
        .map_err(|e| AppError::io("cannot create temporary file", parent, e))?;
    tmp.write_all(bytes)
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|e| AppError::io("cannot write", path, e))?;
    tmp.persist(path)
        .map_err(|e| AppError::io("cannot replace", path, e.error))?;
    Ok(())
}

/// Rename, retrying a few times on Windows when antivirus software or another
/// process briefly holds a file.
pub fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let attempts = if cfg!(windows) { 6 } else { 1 };
    let mut last = None;
    for i in 0..attempts {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && i + 1 < attempts => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(150 * (i as u64 + 1)));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("rename failed")))
}

/// Copy a folder tree. Symlinks are refused, never followed.
pub fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_symlink() {
            return Err(std::io::Error::other(format!(
                "refusing to copy symlink {}",
                entry.path().display()
            )));
        } else if ty.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Move a folder: rename when possible, otherwise copy then delete the source.
pub fn move_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match rename_with_retry(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            if dst.exists() {
                std::fs::remove_dir_all(dst)?;
            }
            copy_dir(src, dst)?;
            std::fs::remove_dir_all(src)
        }
    }
}

/// Remove a folder if it exists.
pub fn remove_dir_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Nearest existing ancestor of a path (for folders that may not exist yet).
pub fn existing_ancestor(path: &Path) -> Option<std::path::PathBuf> {
    let mut p = Some(path);
    while let Some(cur) = p {
        if cur.exists() {
            return Some(cur.to_path_buf());
        }
        p = cur.parent();
    }
    None
}

/// Free bytes available to this user on the volume holding `path`.
pub fn available_space(path: &Path) -> Option<u64> {
    fs4::available_space(existing_ancestor(path)?).ok()
}

pub const SPACE_MARGIN: u64 = 64 * 1024 * 1024;

/// Fail early when the volume of `path` has less than `needed` bytes (+ margin) free.
/// When the free space cannot be read, the check is skipped.
pub fn ensure_space(path: &Path, needed: u64, what: &str) -> Result<()> {
    let Some(free) = available_space(path) else {
        return Ok(());
    };
    let needed = needed.saturating_add(SPACE_MARGIN);
    if free < needed {
        return Err(AppError::Other(format!(
            "not enough free disk space for {what} on the volume of {}: {} MB free, about {} MB needed",
            path.display(),
            free / (1024 * 1024),
            needed / (1024 * 1024)
        )));
    }
    Ok(())
}

/// Total size in bytes of the files under a folder.
pub fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}
