//! Config, data and work dir locations.

use std::path::{Path, PathBuf};

use crate::domain::ProfileName;
use crate::error::{AppError, Result};

pub const ENV_CONFIG: &str = "WOWADDONS_CONFIG";
pub const ENV_DATA_DIR: &str = "WOWADDONS_DATA_DIR";
pub const WORK_DIR_NAME: &str = ".wowaddons-work";

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
}

impl AppPaths {
    /// `--config` > `WOWADDONS_CONFIG` > platform default.
    /// `WOWADDONS_DATA_DIR` > platform default.
    pub fn resolve(config_override: Option<&Path>) -> Result<Self> {
        let config_file = match config_override {
            Some(p) => p.to_path_buf(),
            None => match std::env::var_os(ENV_CONFIG).filter(|v| !v.is_empty()) {
                Some(p) => PathBuf::from(p),
                None => dirs::config_dir()
                    .ok_or_else(|| AppError::Other("cannot find the user config folder".into()))?
                    .join("wowaddons")
                    .join("config.toml"),
            },
        };
        let data_dir = match std::env::var_os(ENV_DATA_DIR).filter(|v| !v.is_empty()) {
            Some(p) => PathBuf::from(p),
            None => dirs::data_local_dir()
                .ok_or_else(|| AppError::Other("cannot find the user data folder".into()))?
                .join("wowaddons"),
        };
        Ok(AppPaths {
            config_file,
            data_dir,
        })
    }

    pub fn profile_dir(&self, profile: &ProfileName) -> PathBuf {
        self.data_dir.join("profiles").join(profile.as_str())
    }

    pub fn state_file(&self, profile: &ProfileName) -> PathBuf {
        self.profile_dir(profile).join("state.json")
    }

    pub fn lock_file(&self, profile: &ProfileName) -> PathBuf {
        self.profile_dir(profile).join("lock")
    }

    /// Archives downloaded by hand, waiting for `wowaddons import`.
    pub fn inbox_dir(&self, profile: &ProfileName) -> PathBuf {
        self.profile_dir(profile).join("inbox")
    }

    pub fn journal_dir(&self, profile: &ProfileName) -> PathBuf {
        self.profile_dir(profile).join("journal")
    }

    pub fn backups_dir(&self, profile: &ProfileName) -> PathBuf {
        self.data_dir.join("backups").join(profile.as_str())
    }

    pub fn config_lock_file(&self) -> PathBuf {
        let mut name = self
            .config_file
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "config.toml".into());
        name.push(".lock");
        self.config_file.with_file_name(name)
    }
}

/// Default work dir: `<parent of addons_path>/.wowaddons-work/<profile>/`.
pub fn default_work_dir(addons_path: &Path, profile: &ProfileName) -> PathBuf {
    addons_path
        .parent()
        .unwrap_or(addons_path)
        .join(WORK_DIR_NAME)
        .join(profile.as_str())
}

/// Canonical absolute path, without the Windows `\\?\` prefix when it is not needed.
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = std::fs::canonicalize(path)?;
    Ok(simplify_verbatim(canonical))
}

#[cfg(windows)]
fn simplify_verbatim(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        let is_drive = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\';
        if is_drive && rest.len() < 250 {
            return PathBuf::from(rest);
        }
    }
    path
}

#[cfg(not(windows))]
fn simplify_verbatim(path: PathBuf) -> PathBuf {
    path
}

/// Key used to compare two paths for equality (case-insensitive on Windows).
pub fn path_key(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/').to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}
