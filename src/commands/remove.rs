//! `wowaddons remove`: delete addons from the `AddOns` folder. Works offline.
//!
//! A removal is a normal transaction: the folders are moved aside, the state entry is
//! dropped, and the folders are kept as a backup, so `rollback` can bring them back.
//! Explicit links (`link`) are kept, so a reinstalled folder is identified again.

use serde_json::json;
use time::OffsetDateTime;

use crate::cli::RemoveArgs;
use crate::domain::Tracking;
use crate::error::{AppError, Result};
use crate::installer::{
    self, Journal, JournalItem, TxHooks, TxKind, TxStatus, new_transaction_id, probe_same_volume,
};
use crate::output::plural;
use crate::scanner::{self, AddonEntry, build_entries};
use crate::storage::FileLock;
use crate::storage::fsutil::{dir_size, ensure_space};

use super::check::select_entries;
use super::update::TxDirGuard;
use super::{Ctx, Outcome, wow_running};

fn version(entry: &AddonEntry) -> Option<String> {
    entry
        .managed
        .as_ref()
        .map(|m| m.display_version.clone())
        .or(entry.local_version.clone())
}

fn plan_json(entry: &AddonEntry) -> serde_json::Value {
    json!({
        "addon": entry.name,
        "project": entry.project,
        "tracking": entry.tracking,
        "version": version(entry),
        "remove": entry.folders,
        "forget": entry.missing_folders,
    })
}

fn plan_lines(entry: &AddonEntry) -> Vec<String> {
    let mut lines = vec![format!(
        "  REMOVE     {}  {}  ({})",
        entry.name,
        version(entry).unwrap_or_else(|| "-".into()),
        entry.tracking
    )];
    if !entry.folders.is_empty() {
        lines.push(format!("             remove: {}", entry.folders.join(", ")));
    }
    if !entry.missing_folders.is_empty() {
        lines.push(format!(
            "             already gone: {}",
            entry.missing_folders.join(", ")
        ));
    }
    lines
}

pub fn run(ctx: &Ctx, args: &RemoveArgs) -> Result<Outcome> {
    let config = ctx.load_config()?;
    let settings = config.settings();
    let profile = ctx.profile(&config)?;
    let txc = ctx.tx_context(&profile, &settings);
    let _lock = FileLock::acquire(&ctx.paths.lock_file(&profile.name), profile.name.as_str())?;
    for msg in installer::recover(&txc)? {
        ctx.out.warn(&msg);
    }

    let state = ctx.load_state(&profile)?;
    let inventory = scanner::scan(&profile.addons_path, &profile.toc_suffixes())?;
    let entries = select_entries(build_entries(&inventory, &state), &args.addons)?;

    // Checked before anything changes: one refused addon stops the whole run.
    for e in &entries {
        if e.tracking == Tracking::Protected {
            return Err(AppError::Blocked(format!(
                "{} is a protected folder (symbolic link or Blizzard folder); it is never touched",
                e.name
            )));
        }
        for f in &e.folders {
            let path = profile.addons_path.join(f);
            let real_dir = std::fs::symlink_metadata(&path)
                .map(|m| m.is_dir())
                .unwrap_or(false);
            if !real_dir {
                return Err(AppError::Blocked(format!(
                    "\"{f}\" of {} is not a plain folder; refusing to remove it",
                    e.name
                )));
            }
        }
    }

    let mut text = vec![
        format!("Profile: {}", profile.name),
        if args.dry_run {
            "Plan (dry run):".to_string()
        } else {
            "Plan:".to_string()
        },
    ];
    for e in &entries {
        text.extend(plan_lines(e));
    }
    if !settings.backup_enabled {
        text.push("Backups are disabled: this removal cannot be rolled back.".into());
    }
    ctx.out.print(&text.join("\n"));
    if wow_running() {
        ctx.out.warn(
            "World of Warcraft seems to be running; changes take effect after /reload or a restart",
        );
    }
    let plan: Vec<_> = entries.iter().map(plan_json).collect();
    if args.dry_run {
        ctx.out.print("Dry run: nothing was changed.");
        return Ok(Outcome::ok(json!({
            "profile": profile.name,
            "dry_run": true,
            "plan": plan,
        })));
    }
    let question = format!("Remove {}?", plural(entries.len(), "addon"));
    if !ctx
        .out
        .confirm(&question, args.yes || settings.auto_confirm)?
    {
        return Err(AppError::Cancelled);
    }

    // --- Transaction ----------------------------------------------------------
    let tx_id = new_transaction_id();
    let tx_dir = txc.tx_dir(&tx_id);
    let mut guard = TxDirGuard(Some(tx_dir.clone()));
    probe_same_volume(&txc.work_dir, &profile.addons_path, &tx_id)?;
    if settings.backup_enabled {
        let size: u64 = entries
            .iter()
            .flat_map(|e| &e.folders)
            .map(|f| dir_size(&profile.addons_path.join(f)))
            .sum();
        ensure_space(&txc.backups_dir(), size, "backups")?;
    }
    let items: Vec<JournalItem> = entries
        .iter()
        .enumerate()
        .map(|(n, e)| JournalItem {
            project: e.project.clone(),
            addon: e.name.clone(),
            action: "REMOVE".into(),
            display_from: version(e),
            display_to: None,
            staging: tx_dir.join("staging").join(n.to_string()),
            move_aside: e.folders.clone(),
            removed: e.folders.clone(),
            install: Vec::new(),
            previous: e.managed.clone(),
            next: None,
        })
        .collect();
    let journal = Journal {
        schema_version: installer::journal::JOURNAL_SCHEMA_VERSION,
        id: tx_id.clone(),
        profile: profile.name.to_string(),
        kind: TxKind::Remove,
        status: TxStatus::Prepared,
        created_at: OffsetDateTime::now_utc(),
        finished_at: None,
        addons_path: profile.addons_path.clone(),
        tx_dir,
        items,
        steps: Vec::new(),
        had_state: false,
        backup_dir: None,
        reverts: None,
        reverted_by: None,
        error: None,
        path: txc.journal_path(&tx_id),
    };
    guard.0 = None; // the transaction owns the folder from now on
    let outcome = installer::execute(&txc, journal, &TxHooks::default())?;

    for w in &outcome.warnings {
        ctx.out.warn(w);
    }
    let mut done = vec![format!(
        "Removed {} (transaction {}).",
        plural(entries.len(), "addon"),
        outcome.journal.id
    )];
    if let Some(b) = &outcome.journal.backup_dir {
        done.push(format!(
            "Backup: {}\nUndo with `wowaddons rollback`.",
            b.display()
        ));
    }
    ctx.out.print(&done.join("\n"));
    Ok(Outcome::ok(json!({
        "profile": profile.name,
        "dry_run": false,
        "transaction_id": outcome.journal.id,
        "removed": plan,
        "backup_dir": outcome.journal.backup_dir,
        "warnings": outcome.warnings,
    })))
}
