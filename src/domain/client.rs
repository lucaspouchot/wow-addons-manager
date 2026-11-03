//! WoW client variants and the client table.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WowClient {
    Forever,
    Retail,
    Classic,
    ClassicEra,
    /// Any other variant. Needs an explicit `game_version_type_id` and `toc_suffixes`.
    Custom,
}

/// Static data for a known client. CurseForge and client facts
/// live here and nowhere else.
pub struct ClientInfo {
    pub client: WowClient,
    pub id: &'static str,
    pub display: &'static str,
    /// TOC suffixes, most specific first.
    pub toc_suffixes: &'static [&'static str],
    /// Normalized names/slugs (lowercase, alphanumeric only) that identify the
    /// client in the CurseForge version-type list.
    pub version_type_aliases: &'static [&'static str],
}

pub const CLIENTS: &[ClientInfo] = &[
    ClientInfo {
        client: WowClient::Forever,
        id: "forever",
        display: "WoW Forever",
        toc_suffixes: &["Camelot"],
        version_type_aliases: &["forever", "wowforever", "camelot", "wowcamelot"],
    },
    ClientInfo {
        client: WowClient::Retail,
        id: "retail",
        display: "WoW Retail",
        toc_suffixes: &["Mainline"],
        version_type_aliases: &["retail", "wowretail"],
    },
    ClientInfo {
        client: WowClient::Classic,
        id: "classic",
        display: "WoW Classic",
        toc_suffixes: &["Mists", "Classic"],
        // CurseForge: "WoW Mists of Pandaria Classic" (79434).
        version_type_aliases: &[
            "mistsclassic",
            "wowmistsclassic",
            "mistsofpandariaclassic",
            "wowmistsofpandariaclassic",
        ],
    },
    ClientInfo {
        client: WowClient::ClassicEra,
        id: "classic-era",
        display: "WoW Classic Era",
        toc_suffixes: &["Vanilla"],
        // CurseForge names Classic Era (1.15.x) plain "WoW Classic" (67408).
        version_type_aliases: &["classicera", "wowclassicera", "wowclassic"],
    },
    ClientInfo {
        client: WowClient::Custom,
        id: "custom",
        display: "Custom client",
        toc_suffixes: &[],
        version_type_aliases: &[],
    },
];

impl WowClient {
    pub fn info(self) -> &'static ClientInfo {
        CLIENTS
            .iter()
            .find(|c| c.client == self)
            .expect("every client has an entry in CLIENTS")
    }

    pub fn id(self) -> &'static str {
        self.info().id
    }

    pub fn display(self) -> &'static str {
        self.info().display
    }

    pub fn known_ids() -> Vec<&'static str> {
        CLIENTS.iter().map(|c| c.id).collect()
    }
}

impl fmt::Display for WowClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

impl FromStr for WowClient {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().to_ascii_lowercase();
        CLIENTS
            .iter()
            .find(|c| c.id == s)
            .map(|c| c.client)
            .ok_or_else(|| {
                format!(
                    "unknown client \"{s}\" (expected one of: {})",
                    WowClient::known_ids().join(", ")
                )
            })
    }
}

/// Lowercase and keep only ASCII letters and digits.
pub fn normalize_alias(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_display() {
        for info in CLIENTS {
            let parsed: WowClient = info.id.parse().unwrap();
            assert_eq!(parsed, info.client);
            assert_eq!(parsed.to_string(), info.id);
        }
        assert!("wotlk".parse::<WowClient>().is_err());
        assert_eq!(
            "Classic-Era".parse::<WowClient>().unwrap(),
            WowClient::ClassicEra
        );
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_alias("WoW Classic-Era"), "wowclassicera");
    }
}
