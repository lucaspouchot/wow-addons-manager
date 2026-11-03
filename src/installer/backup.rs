//! Backup store retention.

use std::path::{Path, PathBuf};

use crate::storage::fsutil::remove_dir_if_exists;

/// Backup folders of a profile, newest first (transaction IDs sort by time).
pub fn list_backups(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.reverse();
    out
}

/// Keep the newest `retention` backups. Returns warnings.
pub fn apply_retention(dir: &Path, retention: u32) -> Vec<String> {
    list_backups(dir)
        .into_iter()
        .skip(retention.max(1) as usize)
        .filter_map(|old| {
            remove_dir_if_exists(&old)
                .err()
                .map(|e| format!("cannot delete old backup {}: {e}", old.display()))
        })
        .collect()
}

pub fn backup_folders(backups_dir: &Path, tx_id: &str) -> PathBuf {
    backups_dir.join(tx_id).join("folders")
}
