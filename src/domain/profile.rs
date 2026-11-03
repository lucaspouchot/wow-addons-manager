//! Profiles.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::Serialize;

use super::client::WowClient;
use super::release::ReleaseChannel;

/// A validated profile name: `^[a-z0-9][a-z0-9_-]{0,31}$`. Safe in file paths.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct ProfileName(String);

impl ProfileName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ProfileName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = s.as_bytes();
        let valid = !bytes.is_empty()
            && bytes.len() <= 32
            && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
            && bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-');
        if valid {
            Ok(ProfileName(s.to_string()))
        } else {
            Err(format!(
                "invalid profile name \"{s}\": use 1 to 32 lowercase letters, digits, '-' or '_', \
                 starting with a letter or digit"
            ))
        }
    }
}

/// A fully resolved profile.
#[derive(Clone, Debug, Serialize)]
pub struct GameProfile {
    pub name: ProfileName,
    pub client: WowClient,
    pub addons_path: PathBuf,
    pub work_dir: Option<PathBuf>,
    pub game_version: Option<String>,
    pub game_version_type_id: Option<u32>,
    pub channel: Option<ReleaseChannel>,
    pub toc_suffixes: Vec<String>,
}

impl GameProfile {
    /// TOC suffixes to try, most specific first.
    pub fn toc_suffixes(&self) -> Vec<String> {
        if self.client == WowClient::Custom {
            self.toc_suffixes.clone()
        } else {
            self.client
                .info()
                .toc_suffixes
                .iter()
                .map(|s| s.to_string())
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["forever", "retail-2", "a", "x_y", "0abc"] {
            assert!(ok.parse::<ProfileName>().is_ok(), "{ok}");
        }
        for bad in ["", "Forever", "-a", "a b", "a/b", "..", &"a".repeat(33)] {
            assert!(bad.parse::<ProfileName>().is_err(), "{bad}");
        }
    }
}
