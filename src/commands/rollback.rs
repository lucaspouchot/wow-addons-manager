//! `wowaddons history` and `wowaddons rollback`.

use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::cli::RollbackArgs;
use crate::error::{AppError, Result};
use crate::installer::backup::backup_folders;
use crate::installer::{
    self, Journal, JournalItem, TxHooks, TxKind, TxStatus, list_journals, new_transaction_id,
    probe_same_volume,
};
use crate::output::table;
use crate::storage::FileLock;
use crate::storage::fsutil::{copy_dir, dir_size, ensure_space, remove_dir_if_exists};

use super::update::state_matches;
use super::{Ctx, Outcome};

fn fmt_date(d: OffsetDateTime) -> String {
    d.format(&Rfc3339).unwrap_or_default()
}

pub fn history(ctx: &Ctx) -> Result<Outcome> {
    let config = ctx.load_config()?;
    let profile = ctx.profile(&config)?;
    let journals = list_journals(&ctx.paths.journal_dir(&profile.name))?;
    let backups = ctx.paths.backups_dir(&profile.name);
    let mut rows = Vec::new();
    let mut items = Vec::new();
    for j in journals.iter().rev() {
        let addons: Vec<String> = j
            .items
            .iter()
            .map(|i| {
                format!(
                    "{} {}",
                    i.addon,
                    i.display_to.clone().unwrap_or_else(|| "-".into())
                )
            })
            .collect();
        let has_backup = backup_folders(&backups, &j.id).exists();
        let mut status = format!("{:?}", j.status).to_uppercase();
        if let Some(r) = &j.reverted_by {
            status = format!("REVERTED by {r}");
        }
        rows.push(vec![
            j.id.clone(),
            format!("{:?}", j.kind).to_lowercase(),
            status.clone(),
            if has_backup {
                "yes".into()
            } else {
                "no".into()
            },
            addons.join(", "),
        ]);
        items.push(json!({
            "id": j.id, "kind": j.kind, "status": j.status, "created_at": fmt_date(j.created_at),
            "reverted_by": j.reverted_by, "reverts": j.reverts, "backup_available": has_backup,
            "error": j.error,
            "addons": j.items.iter().map(|i| json!({
                "addon": i.addon, "project": i.project, "action": i.action,
                "from": i.display_from, "to": i.display_to,
            })).collect::<Vec<_>>(),
        }));
    }
    if rows.is_empty() {
        ctx.out.print(&format!(
            "No transactions for profile \"{}\".",
            profile.name
        ));
    } else {
        ctx.out.print(&table(
            &["TRANSACTION", "KIND", "STATUS", "BACKUP", "ADDONS"],
            rows,
        ));
    }
    Ok(Outcome::ok(
        json!({ "profile": profile.name, "transactions": items }),
    ))
}

pub fn rollback(ctx: &Ctx, args: &RollbackArgs) -> Result<Outcome> {
    let config = ctx.load_config()?;
    let settings = config.settings();
    let profile = ctx.profile(&config)?;
    let txc = ctx.tx_context(&profile, &settings);
    let _lock = FileLock::acquire(&ctx.paths.lock_file(&profile.name), profile.name.as_str())?;
    for msg in installer::recover(&txc)? {
        ctx.out.warn(&msg);
    }

    let journals = list_journals(&txc.journal_dir())?;
    let target = journals
        .iter()
        .rev()
        .find(|j| j.kind.is_revertible() && j.status.is_committed() && j.reverted_by.is_none())
        .cloned()
        .ok_or_else(|| {
            AppError::Other(format!(
                "nothing to roll back for profile \"{}\"",
                profile.name
            ))
        })?;
    if let Some(id) = &args.transaction_id
        && *id != target.id
    {
        return Err(if journals.iter().any(|j| &j.id == id) {
            AppError::Blocked(format!(
                "only the most recent transaction ({}) can be rolled back",
                target.id
            ))
        } else {
            AppError::Usage(format!(
                "unknown transaction \"{id}\" (see `wowaddons history`)"
            ))
        });
    }
    let backup = backup_folders(&txc.backups_dir(), &target.id);
    if !backup.is_dir() {
        return Err(AppError::Blocked(format!(
            "the backup of transaction {} is no longer available",
            target.id
        )));
    }

    // The folders must be exactly as the transaction left them.
    let state = ctx.load_state(&profile)?;
    for item in &target.items {
        let present = item.install.iter().all(|f| {
            let p = profile.addons_path.join(f);
            p.is_dir() && !p.is_symlink()
        });
        // Folders the transaction removed must still be gone, or restoring them would collide.
        let removed_gone = item
            .move_aside
            .iter()
            .filter(|f| !item.install.iter().any(|i| i.eq_ignore_ascii_case(f)))
            .all(|f| std::fs::symlink_metadata(profile.addons_path.join(f)).is_err());
        if !state_matches(&state, item) || !present || !removed_gone {
            return Err(AppError::Blocked(format!(
                "{} has changed since transaction {}; refusing to roll back",
                item.addon, target.id
            )));
        }
        for f in &item.move_aside {
            if !backup.join(f).is_dir() {
                return Err(AppError::Blocked(format!(
                    "backup of folder \"{f}\" is missing"
                )));
            }
        }
    }

    let mut text = vec![format!(
        "Revert transaction {} (profile {}):",
        target.id, profile.name
    )];
    for item in &target.items {
        text.push(format!(
            "  REVERT     {}  {} -> {}",
            item.addon,
            item.display_to.as_deref().unwrap_or("-"),
            item.display_from.as_deref().unwrap_or("(not managed)")
        ));
        if !item.install.is_empty() {
            text.push(format!("             remove: {}", item.install.join(", ")));
        }
        if !item.move_aside.is_empty() {
            text.push(format!(
                "             restore: {}",
                item.move_aside.join(", ")
            ));
        }
    }
    ctx.out.print(&text.join("\n"));
    if args.dry_run {
        ctx.out.print("Dry run: nothing was changed.");
        return Ok(Outcome::ok(
            json!({ "profile": profile.name, "dry_run": true, "transaction_id": target.id }),
        ));
    }
    if !ctx
        .out
        .confirm("Roll back?", args.yes || settings.auto_confirm)?
    {
        return Err(AppError::Cancelled);
    }

    let id = new_transaction_id();
    let tx_dir = txc.tx_dir(&id);
    probe_same_volume(&txc.work_dir, &profile.addons_path, &id)?;
    ensure_space(&txc.work_dir, dir_size(&backup), "restoring the backup")?;
    let mut items = Vec::new();
    for (n, item) in target.items.iter().enumerate() {
        let staging = tx_dir.join("staging").join(n.to_string());
        for f in &item.move_aside {
            if let Err(e) = copy_dir(&backup.join(f), &staging.join(f)) {
                let _ = remove_dir_if_exists(&tx_dir);
                return Err(AppError::io("cannot copy the backup", &backup.join(f), e));
            }
        }
        items.push(JournalItem {
            project: item.project.clone(),
            addon: item.addon.clone(),
            action: "REVERT".into(),
            display_from: item.display_to.clone(),
            display_to: item.display_from.clone(),
            staging,
            move_aside: item.install.clone(),
            removed: item
                .install
                .iter()
                .filter(|f| !item.move_aside.contains(f))
                .cloned()
                .collect(),
            install: item.move_aside.clone(),
            previous: item.next.clone(),
            next: item.previous.clone(),
        });
    }
    let journal = Journal {
        schema_version: installer::journal::JOURNAL_SCHEMA_VERSION,
        id: id.clone(),
        profile: profile.name.to_string(),
        kind: TxKind::Rollback,
        status: TxStatus::Prepared,
        created_at: OffsetDateTime::now_utc(),
        finished_at: None,
        addons_path: profile.addons_path.clone(),
        tx_dir,
        items,
        steps: Vec::new(),
        had_state: false,
        backup_dir: None,
        reverts: Some(target.id.clone()),
        reverted_by: None,
        error: None,
        path: txc.journal_path(&id),
    };
    let outcome = installer::execute(&txc, journal, &TxHooks::default())?;
    let mut target = target;
    target.reverted_by = Some(outcome.journal.id.clone());
    target.save()?;
    for w in &outcome.warnings {
        ctx.out.warn(w);
    }
    ctx.out.print(&format!(
        "Rolled back transaction {} (new transaction {}).",
        target.id, outcome.journal.id
    ));
    Ok(Outcome::ok(json!({
        "profile": profile.name,
        "dry_run": false,
        "reverted": target.id,
        "transaction_id": outcome.journal.id,
    })))
}
