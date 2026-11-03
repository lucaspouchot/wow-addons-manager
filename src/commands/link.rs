//! `wowaddons link` and `wowaddons unlink`.

use serde_json::json;

use crate::cli::{LinkArgs, UnlinkArgs};
use crate::domain::ProjectRef;
use crate::domain::addon::is_curseforge_id;
use crate::error::{AppError, Result};
use crate::providers::AddonProvider;
use crate::scanner;
use crate::storage::FileLock;

use super::{Ctx, Outcome};

pub async fn link(ctx: &Ctx<'_>, args: &LinkArgs) -> Result<Outcome> {
    if !is_curseforge_id(&args.project_id) {
        return Err(AppError::Usage(format!(
            "invalid project ID \"{}\"",
            args.project_id
        )));
    }
    let project = ProjectRef::curseforge(&args.project_id);
    let config = ctx.load_config()?;
    let settings = config.settings();
    let profile = ctx.profile(&config)?;
    let _lock = FileLock::acquire(&ctx.paths.lock_file(&profile.name), profile.name.as_str())?;
    let inventory = scanner::scan(&profile.addons_path, &profile.toc_suffixes())?;
    let addon = inventory
        .find(&args.addon)
        .ok_or_else(|| AppError::Usage(format!("unknown addon folder \"{}\"", args.addon)))?;
    if addon.protected.is_some() {
        return Err(AppError::Blocked(format!(
            "{} is a protected folder",
            addon.folder
        )));
    }
    let state_path = ctx.paths.state_file(&profile.name);
    let mut state = ctx.load_state(&profile)?;
    if let Some((owner, _)) = state.owner_of(&addon.folder)
        && *owner != project
    {
        return Err(AppError::Blocked(format!(
            "{} is managed as {owner}; it cannot be linked to another project",
            addon.folder
        )));
    }

    let mut name = None;
    if ctx.global.offline {
        ctx.out.warn("--offline: the project was not checked");
    } else {
        let provider = ctx.provider(&settings)?;
        let found = provider
            .get_projects(std::slice::from_ref(&project))
            .await?;
        let info = found
            .into_iter()
            .find(|p| p.project == project)
            .ok_or_else(|| {
                AppError::Usage(format!(
                    "CurseForge project {} does not exist",
                    args.project_id
                ))
            })?;
        if !info.is_wow {
            return Err(AppError::Usage(format!(
                "CurseForge project {} is not a World of Warcraft project",
                args.project_id
            )));
        }
        name = Some(info.name);
    }

    if let Some((key, _)) = state
        .link_for(&addon.folder)
        .map(|(k, v)| (k.clone(), v.clone()))
    {
        state.links.remove(&key);
    }
    state.links.insert(addon.folder.clone(), project.clone());
    state.save(&state_path)?;
    ctx.out.print(&format!(
        "Linked {} to {project}{}",
        addon.folder,
        name.as_ref().map(|n| format!(" ({n})")).unwrap_or_default()
    ));
    Ok(Outcome::ok(
        json!({ "addon": addon.folder, "project": project, "project_name": name }),
    ))
}

pub fn unlink(ctx: &Ctx, args: &UnlinkArgs) -> Result<Outcome> {
    let config = ctx.load_config()?;
    let profile = ctx.profile(&config)?;
    let _lock = FileLock::acquire(&ctx.paths.lock_file(&profile.name), profile.name.as_str())?;
    let state_path = ctx.paths.state_file(&profile.name);
    let mut state = ctx.load_state(&profile)?;
    let Some(key) = state.link_for(&args.addon).map(|(k, _)| k.clone()) else {
        return Err(AppError::Other(format!(
            "\"{}\" has no explicit link",
            args.addon
        )));
    };
    let project = state.links.remove(&key);
    state.save(&state_path)?;
    ctx.out.print(&format!("Removed the link of {key}"));
    Ok(Outcome::ok(json!({ "addon": key, "project": project })))
}
