//! Release channels and remote files.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::addon::ProjectRef;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseChannel {
    Release,
    Beta,
    Alpha,
}

impl ReleaseChannel {
    fn rank(self) -> u8 {
        match self {
            ReleaseChannel::Release => 1,
            ReleaseChannel::Beta => 2,
            ReleaseChannel::Alpha => 3,
        }
    }

    /// Does a channel setting accept a file of this release type?
    pub fn accepts(self, file_type: ReleaseChannel) -> bool {
        file_type.rank() <= self.rank()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseChannel::Release => "release",
            ReleaseChannel::Beta => "beta",
            ReleaseChannel::Alpha => "alpha",
        }
    }
}

impl fmt::Display for ReleaseChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReleaseChannel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "release" => Ok(ReleaseChannel::Release),
            "beta" => Ok(ReleaseChannel::Beta),
            "alpha" => Ok(ReleaseChannel::Alpha),
            other => Err(format!(
                "invalid channel \"{other}\" (expected release, beta or alpha)"
            )),
        }
    }
}

/// A remote file, independent of the provider's JSON model.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RemoteFile {
    pub id: String,
    pub project: ProjectRef,
    pub display_name: String,
    pub file_name: String,
    pub channel: ReleaseChannel,
    pub game_version_type_ids: Vec<u32>,
    pub game_versions: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub published_at: OffsetDateTime,
    pub size: Option<u64>,
    /// Lowercase hex SHA-1, when the provider gives one.
    pub sha1: Option<String>,
    pub is_available: bool,
    /// The file status is a published one.
    pub is_published: bool,
    pub download_allowed: bool,
}

/// Compare two provider file IDs: numerically when both are numbers.
pub fn cmp_file_ids(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        _ => a.cmp(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_acceptance() {
        use ReleaseChannel::*;
        assert!(Release.accepts(Release));
        assert!(!Release.accepts(Beta));
        assert!(Beta.accepts(Release));
        assert!(Beta.accepts(Beta));
        assert!(!Beta.accepts(Alpha));
        assert!(Alpha.accepts(Alpha) && Alpha.accepts(Beta) && Alpha.accepts(Release));
    }

    #[test]
    fn file_id_order() {
        assert_eq!(cmp_file_ids("9", "10"), std::cmp::Ordering::Less);
    }
}
