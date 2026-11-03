//! Compatibility, release selection and version-type resolution.

pub mod compatibility;

use crate::domain::WowClient;
use crate::domain::client::normalize_alias;
use crate::error::{AppError, Result};
use crate::providers::VersionType;

pub use compatibility::{
    Criteria, Decision, Ineligible, compare, eligibility, identify_installed, select_latest,
};

/// Version types whose name or slug matches the client's aliases.
pub fn matching_version_types(types: &[VersionType], client: WowClient) -> Vec<&VersionType> {
    let aliases = client.info().version_type_aliases;
    types
        .iter()
        .filter(|t| {
            let name = normalize_alias(&t.name);
            let slug = normalize_alias(&t.slug);
            aliases.iter().any(|a| *a == name || *a == slug)
        })
        .collect()
}

/// Resolve the effective version type ID.
///
/// `cli` (from `--game-version-type-id`) wins over `profile`, which wins over aliases.
/// When `cli_client` is given together with an explicit ID, both must agree.
pub fn resolve_version_type(
    types: &[VersionType],
    client: WowClient,
    cli: Option<u32>,
    profile: Option<u32>,
    cli_client_given: bool,
) -> Result<u32> {
    let known = |id: u32| types.iter().any(|t| t.id == id);
    if let Some(id) = cli {
        if !known(id) {
            return Err(AppError::Usage(format!(
                "--game-version-type-id {id} is not a known WoW version type"
            )));
        }
        if cli_client_given && client != WowClient::Custom {
            let matches = matching_version_types(types, client);
            if matches.len() == 1 && matches[0].id != id {
                return Err(AppError::Usage(format!(
                    "--client {client} and --game-version-type-id {id} do not match \
                     (the API maps {client} to {})",
                    matches[0].id
                )));
            }
        }
        return Ok(id);
    }
    if let Some(id) = profile {
        if !known(id) {
            return Err(AppError::Config(format!(
                "game_version_type_id {id} is not a known WoW version type"
            )));
        }
        return Ok(id);
    }
    let matches = matching_version_types(types, client);
    match matches.as_slice() {
        [one] => Ok(one.id),
        [] => Err(AppError::Config(format!(
            "cannot find the CurseForge version type for client \"{client}\"; \
             set it with `wowaddons profile set <profile> game_version_type_id <id>`"
        ))),
        many => Err(AppError::Config(format!(
            "several CurseForge version types match client \"{client}\" ({}); \
             set one with `wowaddons profile set <profile> game_version_type_id <id>`",
            many.iter()
                .map(|t| format!("{} = {}", t.id, t.name))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types() -> Vec<VersionType> {
        vec![
            VersionType {
                id: 517,
                name: "WoW Retail".into(),
                slug: "wow_retail".into(),
            },
            VersionType {
                id: 67408,
                name: "WoW Classic".into(),
                slug: "wow-classic".into(),
            },
            VersionType {
                id: 88568,
                name: "WoW Forever".into(),
                slug: "wow-forever".into(),
            },
        ]
    }

    /// The real `/v1/games/1/version-types` list (checked 2026-11-03).
    fn real_types() -> Vec<VersionType> {
        [
            (
                79434,
                "WoW Mists of Pandaria Classic",
                "wow-mists-of-pandaria-classic",
            ),
            (67408, "WoW Classic", "wow-classic"),
            (517, "WoW Retail", "game"),
            (77522, "WoW Cataclysm Classic", "wow-cataclysm-classic"),
            (
                73713,
                "WoW Wrath of the Lich King Classic",
                "wow-wrath-classic",
            ),
            (81212, "WoW Wrath Titan Reforged", "wow-wrath-titan"),
            (
                73246,
                "WoW Burning Crusade Classic",
                "wow-burning-crusade-classic",
            ),
            (88568, "WoW Forever", "wow-forever"),
        ]
        .into_iter()
        .map(|(id, name, slug)| VersionType {
            id,
            name: name.into(),
            slug: slug.into(),
        })
        .collect()
    }

    #[test]
    fn by_alias() {
        assert_eq!(
            resolve_version_type(&types(), WowClient::Forever, None, None, false).unwrap(),
            88568
        );
        assert_eq!(
            resolve_version_type(&types(), WowClient::Retail, None, None, false).unwrap(),
            517
        );
    }

    #[test]
    fn by_alias_on_real_list() {
        let t = real_types();
        let resolve = |c| resolve_version_type(&t, c, None, None, false).unwrap();
        assert_eq!(resolve(WowClient::Forever), 88568);
        assert_eq!(resolve(WowClient::Retail), 517);
        assert_eq!(resolve(WowClient::Classic), 79434);
        assert_eq!(resolve(WowClient::ClassicEra), 67408);
    }

    #[test]
    fn no_match_asks_for_config() {
        let err =
            resolve_version_type(&types(), WowClient::Classic, None, None, false).unwrap_err();
        assert!(err.to_string().contains("game_version_type_id"));
    }

    #[test]
    fn ambiguous() {
        let mut t = types();
        t.push(VersionType {
            id: 9,
            name: "Forever".into(),
            slug: "forever".into(),
        });
        assert!(resolve_version_type(&t, WowClient::Forever, None, None, false).is_err());
        // An explicit profile value settles it.
        assert_eq!(
            resolve_version_type(&t, WowClient::Forever, None, Some(9), false).unwrap(),
            9
        );
    }

    #[test]
    fn explicit_ids() {
        assert!(
            resolve_version_type(&types(), WowClient::Forever, Some(1234), None, false).is_err()
        );
        assert_eq!(
            resolve_version_type(&types(), WowClient::Forever, Some(517), None, false).unwrap(),
            517
        );
        // --client forever with --game-version-type-id 517: mismatch.
        assert!(resolve_version_type(&types(), WowClient::Forever, Some(517), None, true).is_err());
    }
}
