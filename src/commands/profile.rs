//! `wowaddons profile …`.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::cli::ProfileCommand;
use crate::config::{Config, ConfigEditor, PROFILE_KEYS, get_profile_value};
use crate::domain::{ProfileName, WowClient};
use crate::error::{AppError, Result};
use crate::output::table;
use crate::storage::FileLock;
use crate::storage::fsutil::remove_dir_if_exists;
use crate::storage::paths::{canonicalize, path_key};

use super::{Ctx, Outcome, work_dir};

/// Check that a folder exists, can be read and written; return its canonical form.
fn check_addons_path(path: &Path) -> Result<PathBuf> {
    let meta = std::fs::metadata(path)
        .map_err(|e| AppError::io("cannot access the AddOns folder", path, e))?;
    if !meta.is_dir() {
        return Err(AppError::Usage(format!(
            "{} is not a folder",
            path.display()
        )));
    }
    std::fs::read_dir(path).map_err(|e| AppError::io("cannot read the AddOns folder", path, e))?;
    let probe = path.join(format!(".wowaddons-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"")
        .map_err(|e| AppError::io("cannot write to the AddOns folder", path, e))?;
    let _ = std::fs::remove_file(&probe);
    canonicalize(path).map_err(|e| AppError::io("cannot resolve", path, e))
}

/// Refuse a path already used by another profile.
fn check_unique(config: &Config, name: &str, path: &Path) -> Result<()> {
    let key = path_key(path);
    for (other, section) in &config.file.profiles {
        if other == name {
            continue;
        }
        let other_path =
            canonicalize(&section.addons_path).unwrap_or_else(|_| section.addons_path.clone());
        if path_key(&other_path) == key {
            return Err(AppError::Usage(format!(
                "profile \"{other}\" already uses {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn parse_name(name: &str) -> Result<ProfileName> {
    name.parse().map_err(AppError::Usage)
}

pub fn run(ctx: &Ctx, cmd: &ProfileCommand) -> Result<Outcome> {
    match cmd {
        ProfileCommand::Add {
            name,
            client,
            addons_path,
            game_version_type_id,
        } => {
            parse_name(name)?;
            let config = ctx.load_config()?;
            if config.file.profiles.contains_key(name) {
                return Err(AppError::Usage(format!(
                    "profile \"{name}\" already exists"
                )));
            }
            if *client == WowClient::Custom && game_version_type_id.is_none() {
                return Err(AppError::Usage(
                    "a custom client needs --game-version-type-id".into(),
                ));
            }
            let path = check_addons_path(addons_path)?;
            check_unique(&config, name, &path)?;
            let mut entries = vec![
                ("client", client.id().to_string()),
                ("addons_path", path.display().to_string()),
            ];
            if let Some(id) = game_version_type_id {
                entries.push(("game_version_type_id", id.to_string()));
            }
            let mut editor = ConfigEditor::open(&ctx.paths)?;
            editor.add_profile(name, &entries)?;
            let made_default = config.file.default_profile.is_none();
            if made_default {
                editor.set_global("default_profile", name)?;
            }
            editor.save()?;
            ctx.out.print(&format!(
                "Added profile \"{name}\" ({}, {}){}",
                client.display(),
                path.display(),
                if made_default {
                    ", now the default profile"
                } else {
                    ""
                }
            ));
            Ok(Outcome::ok(json!({
                "name": name, "client": client, "addons_path": path, "default": made_default,
            })))
        }
        ProfileCommand::List => {
            let config = ctx.load_config()?;
            let default = config.file.default_profile.clone();
            let mut rows = Vec::new();
            let mut items = Vec::new();
            for (name, p) in &config.file.profiles {
                let is_default = default.as_deref() == Some(name.as_str());
                rows.push(vec![
                    format!("{}{name}", if is_default { "* " } else { "  " }),
                    p.client.clone(),
                    p.addons_path.display().to_string(),
                ]);
                items.push(json!({
                    "name": name, "client": p.client, "addons_path": p.addons_path, "default": is_default,
                }));
            }
            if rows.is_empty() {
                ctx.out
                    .print("No profiles. Add one with `wowaddons profile add`.");
            } else {
                ctx.out
                    .print(&table(&["  NAME", "CLIENT", "ADDONS PATH"], rows));
            }
            Ok(Outcome::ok(
                json!({ "profiles": items, "default_profile": default }),
            ))
        }
        ProfileCommand::Show { name } => {
            let config = ctx.load_config()?;
            let section = config
                .file
                .profiles
                .get(name)
                .ok_or_else(|| AppError::ProfileNotFound(name.clone()))?;
            let profile = config.profile(name)?;
            let settings = config.settings();
            let mut rows = Vec::new();
            let mut values = serde_json::Map::new();
            for spec in PROFILE_KEYS {
                let v = get_profile_value(section, spec.key)?;
                rows.push(vec![
                    spec.key.to_string(),
                    v.clone().unwrap_or_else(|| "-".into()),
                ]);
                values.insert(spec.key.into(), json!(v));
            }
            let channel = profile.channel.unwrap_or(settings.channel);
            let wd = work_dir(&profile);
            rows.push(vec!["(effective channel)".into(), channel.to_string()]);
            rows.push(vec![
                "(effective work dir)".into(),
                wd.display().to_string(),
            ]);
            ctx.out.print(&format!(
                "Profile \"{name}\"\n\n{}",
                table(&["KEY", "VALUE"], rows)
            ));
            Ok(Outcome::ok(json!({
                "name": name, "keys": values, "effective_channel": channel, "effective_work_dir": wd,
                "default": config.file.default_profile.as_deref() == Some(name.as_str()),
            })))
        }
        ProfileCommand::Set {
            name,
            key,
            value,
            yes,
        } => {
            let pname = parse_name(name)?;
            let config = ctx.load_config()?;
            let current = config.profile(name)?;
            let _lock = FileLock::acquire(&ctx.paths.lock_file(&pname), name)?;
            let mut value = value.clone();
            match key.as_str() {
                "addons_path" => {
                    let path = check_addons_path(Path::new(&value))?;
                    check_unique(&config, name, &path)?;
                    value = path.display().to_string();
                    let state = ctx.load_state(&current)?;
                    if !state.projects.is_empty() {
                        ctx.out.warn("this profile has managed addons; their state now refers to the new folder");
                    }
                }
                "client" => {
                    let new: WowClient = value.parse().map_err(AppError::Usage)?;
                    let state = ctx.load_state(&current)?;
                    if new != current.client && !state.projects.is_empty() {
                        ctx.out.warn(&format!(
                            "profile \"{name}\" has {} managed addon(s) installed for {}",
                            state.projects.len(),
                            current.client.display()
                        ));
                        if !ctx.out.confirm("Change the client anyway?", *yes)? {
                            return Err(AppError::Cancelled);
                        }
                    }
                }
                "work_dir" if !Path::new(&value).is_absolute() => {
                    return Err(AppError::Usage("work_dir must be an absolute path".into()));
                }
                _ => {}
            }
            let mut editor = ConfigEditor::open(&ctx.paths)?;
            editor.set_profile(name, key, &value)?;
            let config = editor.save()?;
            let v = get_profile_value(&config.file.profiles[name], key)?;
            ctx.out
                .print(&format!("{name}.{key} = {}", v.as_deref().unwrap_or("-")));
            Ok(Outcome::ok(json!({ "name": name, "key": key, "value": v })))
        }
        ProfileCommand::Unset { name, key } => {
            parse_name(name)?;
            let mut editor = ConfigEditor::open(&ctx.paths)?;
            editor.unset_profile(name, key)?;
            editor.save()?;
            ctx.out.print(&format!("{name}.{key} unset"));
            Ok(Outcome::ok(json!({ "name": name, "key": key })))
        }
        ProfileCommand::Use { name } => {
            let config = ctx.load_config()?;
            config.profile(name)?;
            let mut editor = ConfigEditor::open(&ctx.paths)?;
            editor.set_global("default_profile", name)?;
            editor.save()?;
            ctx.out.print(&format!("Default profile: {name}"));
            Ok(Outcome::ok(json!({ "default_profile": name })))
        }
        ProfileCommand::Remove { name, purge, yes } => {
            let pname = parse_name(name)?;
            let config = ctx.load_config()?;
            config.profile(name)?;
            let _lock = FileLock::acquire(&ctx.paths.lock_file(&pname), name)?;
            let what = if *purge {
                format!("Remove profile \"{name}\" and delete its state, journals and backups?")
            } else {
                format!("Remove profile \"{name}\"? (its AddOns folder is not touched)")
            };
            if !ctx.out.confirm(&what, *yes)? {
                return Err(AppError::Cancelled);
            }
            let mut editor = ConfigEditor::open(&ctx.paths)?;
            editor.remove_profile(name)?;
            editor.save()?;
            if *purge {
                drop(_lock);
                for dir in [ctx.paths.profile_dir(&pname), ctx.paths.backups_dir(&pname)] {
                    remove_dir_if_exists(&dir)
                        .map_err(|e| AppError::io("cannot delete", &dir, e))?;
                }
            }
            ctx.out.print(&format!("Removed profile \"{name}\""));
            Ok(Outcome::ok(json!({ "name": name, "purged": purge })))
        }
    }
}
