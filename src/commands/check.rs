//! `wowaddons check`. Read-only.

use serde_json::json;

use crate::cli::CheckArgs;
use crate::domain::CheckStatus;
use crate::error::{AppError, Exit, Result};
use crate::output::{plural, table};
use crate::scanner::{self, AddonEntry, build_entries};

use super::remote::{evaluate, resolve};
use super::{Ctx, Outcome};

/// Keep the entries named by `--addon`; unknown names are a usage error.
pub fn select_entries(entries: Vec<AddonEntry>, names: &[String]) -> Result<Vec<AddonEntry>> {
    if names.is_empty() {
        return Ok(entries);
    }
    for n in names {
        if !entries.iter().any(|e| e.matches(n)) {
            return Err(AppError::Usage(format!(
                "unknown addon \"{n}\" (see `wowaddons list`)"
            )));
        }
    }
    Ok(entries
        .into_iter()
        .filter(|e| names.iter().any(|n| e.matches(n)))
        .collect())
}

pub async fn run(ctx: &Ctx<'_>, args: &CheckArgs) -> Result<Outcome> {
    let config = ctx.load_config()?;
    let settings = config.settings();
    let profile = ctx.profile(&config)?;
    let state = ctx.load_state(&profile)?;
    let inventory = scanner::scan(&profile.addons_path, &profile.toc_suffixes())?;
    let entries = select_entries(build_entries(&inventory, &state), &args.filter.addons)?;

    let provider = ctx.provider(&settings)?;
    let resolved = resolve(&provider, &profile, &settings, &args.filter, ctx.out).await?;
    let criteria = resolved.criteria(false);
    let evals = evaluate(&provider, &entries, &criteria).await?;

    let count = |s: CheckStatus| evals.iter().filter(|e| e.status == s).count();
    let updates = count(CheckStatus::UpdateAvailable);
    // Managed addons with a newer file that must be downloaded by hand.
    let manual_updates = evals
        .iter()
        .filter(|e| {
            e.status == CheckStatus::DownloadNotAllowed
                && e.entry.managed.is_some()
                && e.available.is_some()
        })
        .count();
    let errors = count(CheckStatus::Error);

    let mut text = vec![
        format!(
            "Profile: {}  (client: {}, version type: {})",
            profile.name,
            resolved.client.display(),
            resolved.version_type_id
        ),
        format!("Channel: {}", resolved.channel),
    ];
    if let Some(v) = &resolved.game_version {
        text.push(format!("Game version: {v}"));
    }
    text.push(String::new());
    let rows: Vec<Vec<String>> = evals
        .iter()
        .map(|e| {
            vec![
                e.addon.clone(),
                e.installed.clone().unwrap_or_else(|| "-".into()),
                e.available
                    .as_ref()
                    .map(|f| f.display_name.clone())
                    .unwrap_or_else(|| "-".into()),
                e.status.to_string(),
            ]
        })
        .collect();
    if rows.is_empty() {
        text.push("No addons found.".into());
    } else {
        text.push(table(&["ADDON", "INSTALLED", "AVAILABLE", "STATUS"], rows));
    }
    for e in evals.iter().filter(|e| e.message.is_some()) {
        text.push(format!(
            "  {}: {}",
            e.addon,
            e.message.as_deref().unwrap_or_default()
        ));
    }
    text.push(String::new());
    text.push(format!("{} available.", plural(updates, "update")));
    let summary = [
        (
            count(CheckStatus::Unmanaged),
            "can be adopted (wowaddons update --adopt)",
        ),
        (
            count(CheckStatus::NoCompatibleRelease),
            "has no release for this client",
        ),
        (
            count(CheckStatus::DownloadNotAllowed),
            "must be downloaded by hand (see the links above)",
        ),
        (
            count(CheckStatus::Missing),
            "is missing folders (wowaddons update --reinstall)",
        ),
        (count(CheckStatus::Untracked), "is not tracked"),
        (count(CheckStatus::Conflict), "has conflicting project IDs"),
        (errors, "could not be checked"),
    ];
    for (n, what) in summary {
        if n > 0 {
            let what = if n > 1 {
                what.replacen("has ", "have ", 1).replacen("is ", "are ", 1)
            } else {
                what.to_string()
            };
            text.push(format!("{} {what}.", plural(n, "addon")));
        }
    }
    ctx.out.print(&text.join("\n"));

    let mut exit = Exit::Success;
    if errors > 0 {
        exit = exit.combine(Exit::Partial);
    }
    if args.fail_on_updates && updates + manual_updates > 0 {
        exit = exit.combine(Exit::UpdatesAvailable);
    }
    Ok(Outcome::with_exit(
        exit,
        json!({
            "profile": profile.name,
            "client": resolved.client,
            "version_type_id": resolved.version_type_id,
            "game_version": resolved.game_version,
            "channel": resolved.channel,
            "addons": evals,
            "summary": { "updates": updates, "manual_updates": manual_updates, "errors": errors },
        }),
    ))
}
