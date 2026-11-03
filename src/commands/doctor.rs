//! `wowaddons doctor`. Reports problems without changing anything.

use std::path::Path;

use serde::Serialize;
use serde_json::json;

use crate::error::{Exit, Result};
use crate::installer;
use crate::output::table;
use crate::providers::AddonProvider;
use crate::resolver;
use crate::secrets::{os_store, resolve_api_key};
use crate::storage::fsutil::{available_space, existing_ancestor};

use super::{Ctx, Outcome, work_dir, wow_running};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Level {
    Ok,
    Warn,
    Fail,
    Skip,
}

#[derive(Debug, Serialize)]
struct Check {
    name: String,
    level: Level,
    detail: String,
}

fn check(name: impl Into<String>, level: Level, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        level,
        detail: detail.into(),
    }
}

#[cfg(unix)]
fn same_volume(a: &Path, b: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    Some(std::fs::metadata(a).ok()?.dev() == std::fs::metadata(b).ok()?.dev())
}

#[cfg(windows)]
fn same_volume(a: &Path, b: &Path) -> Option<bool> {
    use std::path::Component;
    let prefix = |p: &Path| {
        let p = std::fs::canonicalize(p).ok()?;
        match p.components().next()? {
            Component::Prefix(x) => Some(x.as_os_str().to_string_lossy().to_lowercase()),
            _ => None,
        }
    };
    Some(prefix(a)? == prefix(b)?)
}

pub async fn run(ctx: &Ctx<'_>) -> Result<Outcome> {
    let mut checks = Vec::new();
    let config = match ctx.load_config() {
        Ok(c) => {
            let detail = if c.exists {
                format!("{} is valid", c.path.display())
            } else {
                format!(
                    "{} does not exist yet (run `wowaddons init`)",
                    c.path.display()
                )
            };
            checks.push(check(
                "config",
                if c.exists { Level::Ok } else { Level::Warn },
                detail,
            ));
            Some(c)
        }
        Err(e) => {
            checks.push(check("config", Level::Fail, e.to_string()));
            None
        }
    };

    let store = os_store();
    let resolved = resolve_api_key(store.as_ref());
    if let Some(w) = &resolved.warning {
        checks.push(check("api key store", Level::Warn, w.clone()));
    }
    let key = resolved.key;
    checks.push(match &key {
        Some((_, source)) => check(
            "api key",
            Level::Ok,
            format!("from the {}", source.describe(store.as_ref())),
        ),
        None => check(
            "api key",
            Level::Warn,
            "not set; run `wowaddons auth set-key` (network commands are unavailable)",
        ),
    });

    let mut version_types = None;
    if let Some(config) = &config {
        if config.file.profiles.is_empty() {
            checks.push(check(
                "profiles",
                Level::Warn,
                "no profile; add one with `wowaddons profile add`",
            ));
        }
        match &config.file.default_profile {
            Some(p) => checks.push(check("default profile", Level::Ok, p.clone())),
            None if !config.file.profiles.is_empty() => checks.push(check(
                "default profile",
                Level::Warn,
                "none; use --profile or `wowaddons profile use`",
            )),
            None => {}
        }
        if ctx.global.offline {
            checks.push(check("curseforge", Level::Skip, "--offline"));
        } else if key.is_some() {
            let settings = config.settings();
            match ctx.provider(&settings) {
                Ok(provider) => match provider.version_types().await {
                    Ok(types) => {
                        checks.push(check(
                            "curseforge",
                            Level::Ok,
                            format!("API reachable, {} version types", types.len()),
                        ));
                        version_types = Some(types);
                    }
                    Err(e) => checks.push(check("curseforge", Level::Fail, e.to_string())),
                },
                Err(e) => checks.push(check("curseforge", Level::Fail, e.to_string())),
            }
        }

        for name in config.profile_names() {
            let label = format!("profile {name}");
            let profile = match config.profile(&name) {
                Ok(p) => p,
                Err(e) => {
                    checks.push(check(&label, Level::Fail, e.to_string()));
                    continue;
                }
            };
            let path = &profile.addons_path;
            match std::fs::read_dir(path) {
                Ok(_) => {
                    let readonly = std::fs::metadata(path)
                        .map(|m| m.permissions().readonly())
                        .unwrap_or(true);
                    if readonly {
                        checks.push(check(
                            &label,
                            Level::Fail,
                            format!("{} is read-only", path.display()),
                        ));
                    } else {
                        checks.push(check(
                            &label,
                            Level::Ok,
                            format!("{} is accessible", path.display()),
                        ));
                    }
                }
                Err(e) => checks.push(check(
                    &label,
                    Level::Fail,
                    format!("{}: {e}", path.display()),
                )),
            }
            if let Some(free) = available_space(path) {
                let mb = free / (1024 * 1024);
                let level = if mb < 500 { Level::Warn } else { Level::Ok };
                checks.push(check(
                    format!("{label} free space"),
                    level,
                    format!("{mb} MB free"),
                ));
            }
            let wd = work_dir(&profile);
            match existing_ancestor(&wd).and_then(|a| same_volume(&a, path)) {
                Some(true) => checks.push(check(
                    format!("{label} work dir"),
                    Level::Ok,
                    wd.display().to_string(),
                )),
                Some(false) => checks.push(check(
                    format!("{label} work dir"),
                    Level::Fail,
                    format!(
                        "{} is not on the same volume as the AddOns folder",
                        wd.display()
                    ),
                )),
                None => checks.push(check(
                    format!("{label} work dir"),
                    Level::Warn,
                    "cannot compare volumes",
                )),
            }
            if let Some(types) = &version_types {
                match resolver::resolve_version_type(
                    types,
                    profile.client,
                    None,
                    profile.game_version_type_id,
                    false,
                ) {
                    Ok(id) => checks.push(check(
                        format!("{label} version type"),
                        Level::Ok,
                        id.to_string(),
                    )),
                    Err(e) => checks.push(check(
                        format!("{label} version type"),
                        Level::Fail,
                        e.to_string(),
                    )),
                }
            }
            match ctx.load_state(&profile) {
                Ok(s) => checks.push(check(
                    format!("{label} state"),
                    Level::Ok,
                    format!("{} managed, {} links", s.projects.len(), s.links.len()),
                )),
                Err(e) => checks.push(check(format!("{label} state"), Level::Fail, e.to_string())),
            }
            let txc = ctx.tx_context(&profile, &config.settings());
            match installer::pending(&txc) {
                Ok(p) if p.is_empty() => {}
                Ok(p) => {
                    for j in p {
                        let level = if j.status == installer::TxStatus::RollbackFailed {
                            Level::Fail
                        } else {
                            Level::Warn
                        };
                        checks.push(check(
                            format!("{label} transaction"),
                            level,
                            format!(
                                "{} is {:?}{}; the next update or rollback will recover it. Journal: {}",
                                j.id,
                                j.status,
                                j.error.map(|e| format!(" ({e})")).unwrap_or_default(),
                                j.path.display()
                            ),
                        ));
                    }
                }
                Err(e) => checks.push(check(
                    format!("{label} journal"),
                    Level::Fail,
                    e.to_string(),
                )),
            }
        }
    }

    if wow_running() {
        checks.push(check(
            "game",
            Level::Warn,
            "World of Warcraft seems to be running",
        ));
    }

    let rows: Vec<Vec<String>> = checks
        .iter()
        .map(|c| {
            vec![
                c.name.clone(),
                format!("{:?}", c.level).to_uppercase(),
                c.detail.clone(),
            ]
        })
        .collect();
    ctx.out.print(&table(&["CHECK", "STATUS", "DETAIL"], rows));
    let failed = checks.iter().filter(|c| c.level == Level::Fail).count();
    let exit = if failed > 0 {
        Exit::General
    } else {
        Exit::Success
    };
    Ok(Outcome::with_exit(
        exit,
        json!({ "checks": checks, "failed": failed }),
    ))
}
