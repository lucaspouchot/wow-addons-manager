//! Per-profile install state.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::domain::{ProjectRef, WowClient};
use crate::error::{AppError, Result};
use crate::storage::fsutil::atomic_write;

pub const STATE_SCHEMA_VERSION: u32 = 1;

/// What wowaddons installed for one project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedProject {
    /// Remote file ID. `None` for an archive imported from the inbox: the exact
    /// remote file is then found later through `archive_sha1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    /// SHA-1 of the archive imported from the inbox (lowercase hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_sha1: Option<String>,
    pub display_version: String,
    /// Publication date of the installed file. For an imported archive, the import
    /// time: the file cannot have been published later than that.
    #[serde(with = "time::serde::rfc3339")]
    pub file_date: OffsetDateTime,
    /// Main folder first.
    pub folders: Vec<String>,
    pub client: WowClient,
    pub game_version_type_id: Option<u32>,
    #[serde(default)]
    pub client_mismatch: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub installed_at: OffsetDateTime,
    pub transaction_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub schema_version: u32,
    pub profile: String,
    #[serde(default)]
    pub projects: BTreeMap<ProjectRef, ManagedProject>,
    /// Explicit links: folder name → project.
    #[serde(default)]
    pub links: BTreeMap<String, ProjectRef>,
}

impl State {
    pub fn new(profile: &str) -> Self {
        State {
            schema_version: STATE_SCHEMA_VERSION,
            profile: profile.to_string(),
            projects: BTreeMap::new(),
            links: BTreeMap::new(),
        }
    }

    /// Load the state, or an empty one if the file does not exist.
    pub fn load(path: &Path, profile: &str) -> Result<State> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::new(profile)),
            Err(e) => return Err(AppError::io("cannot read state", path, e)),
        };
        let state: State = serde_json::from_str(&text)
            .map_err(|e| AppError::Other(format!("invalid state file {}: {e}", path.display())))?;
        if state.schema_version > STATE_SCHEMA_VERSION {
            return Err(AppError::Other(format!(
                "state file {} was written by a newer version of wowaddons",
                path.display()
            )));
        }
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| AppError::Other(format!("cannot serialize state: {e}")))?;
        atomic_write(path, &json)
    }

    /// Find a link by folder name (case-insensitive).
    pub fn link_for(&self, folder: &str) -> Option<(&String, &ProjectRef)> {
        self.links
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(folder))
    }

    /// The managed project that owns a folder (case-insensitive).
    pub fn owner_of(&self, folder: &str) -> Option<(&ProjectRef, &ManagedProject)> {
        self.projects
            .iter()
            .find(|(_, m)| m.folders.iter().any(|f| f.eq_ignore_ascii_case(folder)))
    }
}
