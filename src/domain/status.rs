//! Addon statuses.

use std::fmt;

use serde::Serialize;

/// Tracking status, known without network access (`list`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Tracking {
    Managed,
    Unmanaged,
    Untracked,
    Protected,
    Missing,
    Conflict,
}

impl Tracking {
    pub fn as_str(self) -> &'static str {
        match self {
            Tracking::Managed => "MANAGED",
            Tracking::Unmanaged => "UNMANAGED",
            Tracking::Untracked => "UNTRACKED",
            Tracking::Protected => "PROTECTED",
            Tracking::Missing => "MISSING",
            Tracking::Conflict => "CONFLICT",
        }
    }
}

impl fmt::Display for Tracking {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Status reported by `check`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CheckStatus {
    UpToDate,
    UpdateAvailable,
    Unmanaged,
    Untracked,
    NoCompatibleRelease,
    DownloadNotAllowed,
    Missing,
    Protected,
    Conflict,
    Error,
}

impl CheckStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckStatus::UpToDate => "UP_TO_DATE",
            CheckStatus::UpdateAvailable => "UPDATE_AVAILABLE",
            CheckStatus::Unmanaged => "UNMANAGED",
            CheckStatus::Untracked => "UNTRACKED",
            CheckStatus::NoCompatibleRelease => "NO_COMPATIBLE_RELEASE",
            CheckStatus::DownloadNotAllowed => "DOWNLOAD_NOT_ALLOWED",
            CheckStatus::Missing => "MISSING",
            CheckStatus::Protected => "PROTECTED",
            CheckStatus::Conflict => "CONFLICT",
            CheckStatus::Error => "ERROR",
        }
    }
}

impl fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
