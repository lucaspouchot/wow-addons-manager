//! Error types and process exit codes.

use std::path::{Path, PathBuf};

/// Process exit codes. The README lists their meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Exit {
    Success = 0,
    General = 1,
    Usage = 2,
    Auth = 3,
    InstallFailed = 4,
    Blocked = 5,
    Partial = 6,
    RollbackFailed = 7,
    Cancelled = 8,
    UpdatesAvailable = 10,
}

impl Exit {
    pub fn code(self) -> u8 {
        self as u8
    }

    /// Higher value wins when several outcomes apply.
    /// Order: 2 > 7 > 4 > 3 > 5 > 6 > 1 > 8 > 10 > 0.
    fn severity(self) -> u8 {
        match self {
            Exit::Success => 0,
            Exit::UpdatesAvailable => 1,
            Exit::Cancelled => 2,
            Exit::General => 3,
            Exit::Partial => 4,
            Exit::Blocked => 5,
            Exit::Auth => 6,
            Exit::InstallFailed => 7,
            Exit::RollbackFailed => 8,
            Exit::Usage => 9,
        }
    }

    /// Keep the most severe of two outcomes.
    pub fn combine(self, other: Exit) -> Exit {
        if other.severity() > self.severity() {
            other
        } else {
            self
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Usage(String),

    #[error("invalid configuration: {0}")]
    Config(String),

    #[error("{message}: {path}: {source}")]
    Io {
        message: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("profile not found: {0}")]
    ProfileNotFound(String),

    #[error("no profile selected")]
    NoProfile,

    #[error("CurseForge API key is missing.")]
    MissingApiKey,

    #[error("authentication refused by {0}")]
    Auth(String),

    #[error("rate limited by the server")]
    RateLimited,

    #[error("network error: {0}")]
    Network(String),

    #[error("{0}")]
    Remote(String),

    #[error("{0}")]
    Blocked(String),

    #[error("another wowaddons process is modifying profile \"{0}\"")]
    Locked(String),

    #[error("{0}")]
    InstallFailed(String),

    #[error("{0}")]
    RollbackFailed(String),

    #[error("cancelled; nothing was changed")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

impl AppError {
    pub fn io(message: impl Into<String>, path: &Path, source: std::io::Error) -> Self {
        AppError::Io {
            message: message.into(),
            path: path.to_path_buf(),
            source,
        }
    }

    pub fn exit(&self) -> Exit {
        match self {
            AppError::Usage(_) => Exit::Usage,
            AppError::MissingApiKey | AppError::Auth(_) => Exit::Auth,
            AppError::Blocked(_) => Exit::Blocked,
            AppError::InstallFailed(_) => Exit::InstallFailed,
            AppError::RollbackFailed(_) => Exit::RollbackFailed,
            AppError::Cancelled => Exit::Cancelled,
            _ => Exit::General,
        }
    }

    /// Stable machine-readable kind, used in JSON output.
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::Usage(_) => "usage",
            AppError::Config(_) => "config",
            AppError::Io { .. } => "io",
            AppError::ProfileNotFound(_) => "profile_not_found",
            AppError::NoProfile => "no_profile",
            AppError::MissingApiKey => "missing_api_key",
            AppError::Auth(_) => "auth",
            AppError::RateLimited => "rate_limited",
            AppError::Network(_) => "network",
            AppError::Remote(_) => "remote",
            AppError::Blocked(_) => "blocked",
            AppError::Locked(_) => "locked",
            AppError::InstallFailed(_) => "install_failed",
            AppError::RollbackFailed(_) => "rollback_failed",
            AppError::Cancelled => "cancelled",
            AppError::Other(_) => "other",
        }
    }

    pub fn hint(&self) -> Option<String> {
        match self {
            AppError::MissingApiKey => Some(
                "run `wowaddons auth set-key` to store it securely, or set WOWADDONS_CURSEFORGE_API_KEY"
                    .into(),
            ),
            AppError::NoProfile => Some(
                "create one with `wowaddons profile add <name> --client <client> --addons-path <path>`, \
                 or select one with --profile"
                    .into(),
            ),
            AppError::ProfileNotFound(_) => Some("list profiles with `wowaddons profile list`".into()),
            AppError::Locked(_) => Some("wait for the other process to finish, then retry".into()),
            AppError::RollbackFailed(_) => Some("run `wowaddons doctor` for details".into()),
            _ => None,
        }
    }
}

pub type Result<T, E = AppError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_order() {
        use Exit::*;
        let order = [
            Success,
            UpdatesAvailable,
            Cancelled,
            General,
            Partial,
            Blocked,
            Auth,
            InstallFailed,
            RollbackFailed,
        ];
        for w in order.windows(2) {
            assert_eq!(w[0].combine(w[1]), w[1]);
            assert_eq!(w[1].combine(w[0]), w[1]);
        }
    }

    #[test]
    fn exit_codes() {
        assert_eq!(Exit::Usage.code(), 2);
        assert_eq!(Exit::UpdatesAvailable.code(), 10);
        assert_eq!(AppError::MissingApiKey.exit(), Exit::Auth);
    }
}
