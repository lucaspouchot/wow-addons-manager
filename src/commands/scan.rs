//! `wowaddons scan` and `wowaddons list`. Read-only, offline.

use std::path::PathBuf;

use serde_json::json;

use crate::cli::InspectArgs;
use crate::domain::Tracking;
use crate::error::{AppError, Result};
use crate::output::{plural, table};
use crate::scanner::{self, AddonEntry, Inventory, build_entries};
use crate::storage::State;

use super::{Ctx, Outcome};

struct Target {
    path: PathBuf,
    profile: Option<String>,
    inventory: Inventory,
    entries: Vec<AddonEntry>,
}

fn load(ctx: &Ctx, args: &InspectArgs) -> Result<Target> {
    let config = ctx.load_config()?;
    if let Some(path) = &args.addons_path {
        if !path.is_dir() {
            return Err(AppError::Usage(format!(
                "{} is not a folder",
                path.display()
            )));
        }
        // Use the profile's client suffixes only when a profile is named.
        let (suffixes, profile) = match &ctx.global.profile {
            Some(name) => {
                let p = config.profile(name)?;
                (p.toc_suffixes(), Some(p.name.to_string()))
            }
            None => (Vec::new(), None),
        };
        let inventory = scanner::scan(path, &suffixes)?;
        let entries = build_entries(&inventory, &State::new(""));
        return Ok(Target {
            path: path.clone(),
            profile,
            inventory,
            entries,
        });
    }
    let profile = ctx.profile(&config)?;
    let inventory = scanner::scan(&profile.addons_path, &profile.toc_suffixes())?;
    let state = ctx.load_state(&profile)?;
    let entries = build_entries(&inventory, &state);
    Ok(Target {
        path: profile.addons_path.clone(),
        profile: Some(profile.name.to_string()),
        inventory,
        entries,
    })
}

pub fn scan(ctx: &Ctx, args: &InspectArgs) -> Result<Outcome> {
    let t = load(ctx, args)?;
    let mut text = Vec::new();
    text.push(format!("AddOns folder: {}", t.path.display()));
    if let Some(p) = &t.profile {
        text.push(format!("Profile: {p}"));
    }
    text.push(String::new());
    for a in &t.inventory.addons {
        text.push(a.folder.clone());
        if let Some(reason) = a.protected {
            text.push(format!(
                "  protected: {}",
                match reason {
                    crate::domain::ProtectedReason::Symlink => "symbolic link or junction",
                    crate::domain::ProtectedReason::Blizzard => "Blizzard folder",
                }
            ));
            continue;
        }
        let interfaces: Vec<String> = a.interface_versions.iter().map(u32::to_string).collect();
        text.push(format!(
            "  title: {}   version: {}   interface: {}",
            a.title.as_deref().unwrap_or("-"),
            a.toc_version.as_deref().unwrap_or("-"),
            if interfaces.is_empty() {
                "-".into()
            } else {
                interfaces.join(", ")
            }
        ));
        text.push(format!(
            "  toc: {} (found: {})",
            a.selected_toc.as_deref().unwrap_or("-"),
            a.toc_files.join(", ")
        ));
        if !a.declared_projects.is_empty() {
            let p: Vec<String> = a.declared_projects.iter().map(|p| p.to_string()).collect();
            text.push(format!("  project: {}", p.join(", ")));
        }
        for w in &a.warnings {
            text.push(format!("  warning: {w}"));
        }
    }
    if !t.inventory.ignored.is_empty() {
        text.push(String::new());
        text.push("Ignored folders:".into());
        for i in &t.inventory.ignored {
            text.push(format!("  {}: {}", i.name, i.reason));
        }
    }
    text.push(String::new());
    text.push(format!(
        "{}, {} ignored.",
        plural(t.inventory.addons.len(), "addon folder"),
        t.inventory.ignored.len()
    ));
    ctx.out.print(&text.join("\n"));
    Ok(Outcome::ok(json!({
        "addons_path": t.path,
        "profile": t.profile,
        "folders": t.inventory.addons,
        "ignored": t.inventory.ignored,
        "addons": t.entries,
    })))
}

pub fn list(ctx: &Ctx, args: &InspectArgs) -> Result<Outcome> {
    let t = load(ctx, args)?;
    let rows: Vec<Vec<String>> = t
        .entries
        .iter()
        .map(|e| {
            vec![
                e.name.clone(),
                e.local_version.clone().unwrap_or_else(|| "-".into()),
                e.project
                    .as_ref()
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".into()),
                e.tracking.to_string(),
            ]
        })
        .collect();
    let count = |t_: Tracking| t.entries.iter().filter(|e| e.tracking == t_).count();
    let mut text = Vec::new();
    if let Some(p) = &t.profile {
        text.push(format!("Profile: {p}"));
    }
    text.push(format!("AddOns folder: {}", t.path.display()));
    text.push(String::new());
    if rows.is_empty() {
        text.push("No addons found.".into());
    } else {
        text.push(table(&["ADDON", "VERSION", "PROJECT", "STATUS"], rows));
    }
    text.push(String::new());
    text.push(format!(
        "{}: {} managed, {} unmanaged, {} untracked, {} other.",
        plural(t.entries.len(), "addon"),
        count(Tracking::Managed),
        count(Tracking::Unmanaged),
        count(Tracking::Untracked),
        count(Tracking::Protected) + count(Tracking::Missing) + count(Tracking::Conflict),
    ));
    ctx.out.print(&text.join("\n"));
    Ok(Outcome::ok(json!({
        "addons_path": t.path,
        "profile": t.profile,
        "addons": t.entries,
    })))
}
