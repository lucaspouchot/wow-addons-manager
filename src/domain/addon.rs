//! Local addons and project references.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    CurseForge,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::CurseForge => "curseforge",
        }
    }
}

/// A project on a remote provider. Written as `<provider>:<id>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectRef {
    pub provider: ProviderKind,
    pub id: String,
}

impl ProjectRef {
    pub fn curseforge(id: impl Into<String>) -> Self {
        ProjectRef {
            provider: ProviderKind::CurseForge,
            id: id.into(),
        }
    }
}

impl fmt::Display for ProjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.provider.as_str(), self.id)
    }
}

impl FromStr for ProjectRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (provider, id) = s
            .split_once(':')
            .ok_or_else(|| format!("invalid project reference \"{s}\""))?;
        match provider {
            "curseforge" if is_curseforge_id(id) => Ok(ProjectRef::curseforge(id)),
            _ => Err(format!("invalid project reference \"{s}\"")),
        }
    }
}

impl Serialize for ProjectRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ProjectRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// CurseForge project and file IDs are positive integers.
pub fn is_curseforge_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 12 && s.bytes().all(|b| b.is_ascii_digit()) && s != "0"
}

/// Why a folder must never be modified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedReason {
    Symlink,
    Blizzard,
}

/// One addon folder found on disk.
#[derive(Clone, Debug, Serialize)]
pub struct LocalAddon {
    pub folder: String,
    pub path: PathBuf,
    pub title: Option<String>,
    pub toc_version: Option<String>,
    pub interface_versions: Vec<u32>,
    pub selected_toc: Option<String>,
    pub toc_files: Vec<String>,
    /// Project IDs found in the folder's TOC files (all of them, deduplicated).
    pub declared_projects: Vec<ProjectRef>,
    pub protected: Option<ProtectedReason>,
    pub warnings: Vec<String>,
}

impl LocalAddon {
    /// The TOC files disagree on the project ID.
    pub fn has_project_conflict(&self) -> bool {
        self.declared_projects.len() > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_ref_round_trip() {
        let p: ProjectRef = "curseforge:111".parse().unwrap();
        assert_eq!(p, ProjectRef::curseforge("111"));
        assert_eq!(p.to_string(), "curseforge:111");
        assert!("curseforge:abc".parse::<ProjectRef>().is_err());
        assert!("wago:1".parse::<ProjectRef>().is_err());
        assert!("111".parse::<ProjectRef>().is_err());
    }
}
