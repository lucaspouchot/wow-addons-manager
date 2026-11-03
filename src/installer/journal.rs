//! Transaction journal. One JSON file per transaction.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::domain::ProjectRef;
use crate::error::{AppError, Result};
use crate::storage::ManagedProject;
use crate::storage::fsutil::atomic_write;

pub const JOURNAL_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TxStatus {
    /// Plan written; `AddOns` may be partly changed. Recovery rolls back.
    Prepared,
    /// New state written. Recovery finishes the post-commit steps.
    Committed,
    /// Done: backups moved, work dir cleaned.
    Finalized,
    /// Failed and restored.
    RolledBack,
    /// Failed and could not be restored: manual action needed.
    RollbackFailed,
}

impl TxStatus {
    pub fn is_committed(self) -> bool {
        matches!(self, TxStatus::Committed | TxStatus::Finalized)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TxKind {
    Update,
    Rollback,
}

/// One filesystem operation, journaled before it runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Step {
    /// `AddOns/<folder>` → `<tx_dir>/previous/<folder>`.
    MoveAside { folder: String },
    /// `<item staging>/<folder>` → `AddOns/<folder>`.
    Install { item: usize, folder: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalItem {
    pub project: ProjectRef,
    pub addon: String,
    pub action: String,
    pub display_from: Option<String>,
    pub display_to: Option<String>,
    /// Folder holding the new folders to install.
    pub staging: PathBuf,
    /// Existing folders to move aside (replaced and removed).
    pub move_aside: Vec<String>,
    /// Folders removed without replacement (subset of `move_aside`, for display).
    pub removed: Vec<String>,
    /// Folders to install from `staging`.
    pub install: Vec<String>,
    /// State entry before the transaction.
    pub previous: Option<ManagedProject>,
    /// State entry after the transaction (`None` removes it).
    pub next: Option<ManagedProject>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Journal {
    pub schema_version: u32,
    pub id: String,
    pub profile: String,
    pub kind: TxKind,
    pub status: TxStatus,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    pub addons_path: PathBuf,
    pub tx_dir: PathBuf,
    pub items: Vec<JournalItem>,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub had_state: bool,
    #[serde(default)]
    pub backup_dir: Option<PathBuf>,
    #[serde(default)]
    pub reverts: Option<String>,
    #[serde(default)]
    pub reverted_by: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(skip)]
    pub path: PathBuf,
}

impl Journal {
    pub fn previous_dir(&self) -> PathBuf {
        self.tx_dir.join("previous")
    }

    pub fn state_backup(&self) -> PathBuf {
        self.tx_dir.join("state.json.bak")
    }

    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| AppError::Other(format!("cannot serialize journal: {e}")))?;
        atomic_write(&self.path, &json)
    }

    pub fn load(path: &Path) -> Result<Journal> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| AppError::io("cannot read journal", path, e))?;
        let mut j: Journal = serde_json::from_str(&text)
            .map_err(|e| AppError::Other(format!("invalid journal {}: {e}", path.display())))?;
        j.path = path.to_path_buf();
        Ok(j)
    }
}

/// All journals of a profile, oldest first (IDs sort by time).
pub fn list_journals(dir: &Path) -> Result<Vec<Journal>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(AppError::io("cannot read journal folder", dir, e)),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            out.push(Journal::load(&path)?);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// A new transaction ID: `YYYYMMDDTHHMMSSZ-xxxx`. Sorts by time.
pub fn new_transaction_id() -> String {
    let now = OffsetDateTime::now_utc();
    let nanos = now.nanosecond() as u64;
    let rand = (nanos ^ (std::process::id() as u64).rotate_left(17)) & 0xffff;
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z-{rand:04x}",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}
