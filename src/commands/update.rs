//! `wowaddons update`.

use std::collections::HashSet;
use std::path::PathBuf;

use futures_util::StreamExt;
use futures_util::stream;
use serde::Serialize;
use serde_json::json;
use time::OffsetDateTime;

use crate::cli::UpdateArgs;
use crate::domain::addon::is_curseforge_id;
use crate::domain::{CheckStatus, RemoteFile, Tracking};
use crate::error::{AppError, Exit, Result};
use crate::installer::{
    self, ArchiveLimits, ExtractedArchive, FolderPlan, Journal, JournalItem, TxHooks, TxKind,
    TxStatus, new_transaction_id, plan_folders, probe_same_volume,
};
use crate::output::plural;
use crate::providers::{AddonProvider, DownloadedArchive, ProviderError};
use crate::resolver::{self, Criteria, Decision};
use crate::scanner::{self, AddonEntry, build_entries};
use crate::storage::fsutil::{dir_size, ensure_space, remove_dir_if_exists};
use crate::storage::{FileLock, ManagedProject, State};

use super::check::select_entries;
use super::remote::{Evaluation, evaluate, resolve};
use super::{Ctx, Outcome, wow_running};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum Action {
    Update,
    Adopt,
    Reinstall,
    Downgrade,
}

struct Candidate {
    entry: AddonEntry,
    file: RemoteFile,
    action: Action,
}

struct Ready {
    cand: Candidate,
    extracted: ExtractedArchive,
    plan: FolderPlan,
    warnings: Vec<String>,
}

/// One line of the final report for addons that are not installed.
#[derive(Debug, Serialize)]
pub(super) struct Skipped {
    addon: String,
    status: String,
    message: String,
}

/// Removes the transaction folder unless disarmed.
pub(super) struct TxDirGuard(pub(super) Option<PathBuf>);

impl Drop for TxDirGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            let _ = remove_dir_if_exists(&dir);
        }
    }
}

fn blocked(entry: &AddonEntry, status: &str, message: impl Into<String>) -> Skipped {
    Skipped {
        addon: entry.name.clone(),
        status: status.into(),
        message: message.into(),
    }
}

pub async fn run(ctx: &Ctx<'_>, args: &UpdateArgs) -> Result<Outcome> {
    if args.file_id.is_some() && args.filter.addons.len() != 1 {
        return Err(AppError::Usage(
            "--file-id needs exactly one --addon".into(),
        ));
    }
    if let Some(id) = &args.file_id
        && !is_curseforge_id(id)
    {
        return Err(AppError::Usage(format!("invalid file ID \"{id}\"")));
    }
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
    let named = !args.filter.addons.is_empty();
    let entries = select_entries(build_entries(&inventory, &state), &args.filter.addons)?;

    let provider = ctx.provider(&settings)?;
    let resolved = resolve(&provider, &profile, &settings, &args.filter, ctx.out).await?;
    if resolved.is_foreign(&profile) && !args.allow_client_mismatch {
        return Err(AppError::Blocked(format!(
            "this run targets {} (version type {}) but profile \"{}\" is {}; \
             pass --allow-client-mismatch to install files for another client",
            resolved.client.display(),
            resolved.version_type_id,
            profile.name,
            profile.client.display()
        )));
    }
    let criteria = resolved.criteria(args.allow_client_mismatch);

    // --- Select what to install -------------------------------------------
    let mut skipped: Vec<Skipped> = Vec::new();
    let mut exit = Exit::Success;
    let mut unmanaged_left = 0usize;
    let mut eligible_entries = Vec::new();
    for e in entries {
        match e.tracking {
            Tracking::Managed => eligible_entries.push(e),
            Tracking::Missing if args.reinstall => eligible_entries.push(e),
            Tracking::Unmanaged if args.adopt => eligible_entries.push(e),
            Tracking::Missing => {
                skipped.push(blocked(
                    &e,
                    "MISSING",
                    "folders are missing; use --reinstall to repair",
                ));
                if named {
                    exit = exit.combine(Exit::Blocked);
                }
            }
            Tracking::Unmanaged => {
                unmanaged_left += 1;
                if named {
                    skipped.push(blocked(
                        &e,
                        "UNMANAGED",
                        "not installed by wowaddons; use --adopt",
                    ));
                    exit = exit.combine(Exit::Blocked);
                }
            }
            other if named => {
                skipped.push(blocked(&e, other.as_str(), "this addon cannot be updated"));
                exit = exit.combine(Exit::Blocked);
            }
            _ => {}
        }
    }

    let candidates = if let Some(file_id) = &args.file_id {
        match eligible_entries.into_iter().next() {
            Some(entry) => {
                vec![explicit_candidate(&provider, entry, file_id, &criteria, args).await?]
            }
            None => Vec::new(),
        }
    } else {
        let evals = evaluate(&provider, &eligible_entries, &criteria).await?;
        let mut out = Vec::new();
        for ev in evals {
            match pick(
                &provider,
                ev,
                &criteria,
                args,
                named,
                &mut skipped,
                &mut exit,
            )
            .await?
            {
                Some(c) => out.push(c),
                None => continue,
            }
        }
        out
    };

    if candidates.is_empty() {
        let mut text = report_skipped(&skipped);
        text.push(nothing_message(unmanaged_left));
        ctx.out.print(&text.join("\n"));
        return Ok(Outcome::with_exit(
            exit,
            json!({ "profile": profile.name, "dry_run": args.dry_run, "installed": [], "skipped": skipped }),
        ));
    }

    // --- Download, verify, extract (no change to AddOns) ---------------------
    let tx_id = new_transaction_id();
    let tx_dir = txc.tx_dir(&tx_id);
    let mut guard = TxDirGuard(Some(tx_dir.clone()));
    probe_same_volume(&txc.work_dir, &profile.addons_path, &tx_id)?;
    // Archive + extracted content; unknown sizes count as 5 MB.
    let expected: u64 = candidates
        .iter()
        .map(|c| c.file.size.unwrap_or(5 * 1024 * 1024).saturating_mul(3))
        .sum();
    ensure_space(&txc.work_dir, expected, "downloads")?;
    let downloads = tx_dir.join("downloads");
    std::fs::create_dir_all(&downloads)
        .map_err(|e| AppError::io("cannot create", &downloads, e))?;

    let max_bytes = settings.max_archive_size_mb * 1024 * 1024;
    let bar = ctx.out.progress(candidates.len() as u64, "Downloading");
    let results: Vec<(usize, std::result::Result<DownloadedArchive, ProviderError>)> =
        stream::iter(candidates.iter().enumerate())
            .map(|(i, c)| {
                let dest = downloads.join(format!("{i}.zip"));
                let provider = &provider;
                let bar = &bar;
                async move {
                    let r = provider.download(&c.file, &dest, max_bytes, &|_| {}).await;
                    bar.inc(1);
                    (i, r)
                }
            })
            .buffer_unordered(settings.max_parallel_downloads.max(1))
            .collect()
            .await;
    bar.finish_and_clear();
    let mut archives: Vec<Option<std::result::Result<DownloadedArchive, ProviderError>>> =
        (0..candidates.len()).map(|_| None).collect();
    for (i, r) in results {
        match r {
            Err(e) if e.is_fatal() => return Err(e.into()),
            r => archives[i] = Some(r),
        }
    }

    let limits = ArchiveLimits {
        max_entries: settings.max_archive_entries,
        max_extracted_bytes: settings.max_extracted_size_mb * 1024 * 1024,
    };
    let mut ready: Vec<Ready> = Vec::new();
    let mut claimed: HashSet<String> = HashSet::new();
    for (i, cand) in candidates.into_iter().enumerate() {
        let archive = match archives[i].take() {
            Some(Ok(a)) => a,
            Some(Err(ProviderError::DownloadNotAllowed)) => {
                skipped.push(blocked(
                    &cand.entry,
                    CheckStatus::DownloadNotAllowed.as_str(),
                    ProviderError::DownloadNotAllowed.to_string(),
                ));
                exit = exit.combine(if named { Exit::Blocked } else { Exit::Partial });
                continue;
            }
            Some(Err(e)) => {
                skipped.push(blocked(
                    &cand.entry,
                    "ERROR",
                    format!("download failed: {e}"),
                ));
                exit = exit.combine(Exit::Partial);
                continue;
            }
            None => continue,
        };
        let mut warnings = Vec::new();
        match installer::verify_download(&archive, &cand.file) {
            Ok(Some(w)) => warnings.push(w),
            Ok(None) => {}
            Err(e) => {
                skipped.push(blocked(
                    &cand.entry,
                    "ERROR",
                    format!("download rejected: {e}"),
                ));
                exit = exit.combine(Exit::Partial);
                continue;
            }
        }
        let staging = tx_dir.join("staging").join(i.to_string());
        let extracted = match installer::extract(&archive.path, &staging, limits) {
            Ok(x) => x,
            Err(e) => {
                skipped.push(blocked(
                    &cand.entry,
                    "ERROR",
                    format!("archive rejected: {e}"),
                ));
                exit = exit.combine(Exit::Partial);
                continue;
            }
        };
        let _ = std::fs::remove_file(&archive.path);
        let project = cand
            .entry
            .project
            .clone()
            .expect("candidates have a project");
        let adopt = cand.entry.managed.is_none();
        let plan = match plan_folders(
            &project,
            &extracted.folders,
            &profile.addons_path,
            &inventory,
            &state,
            adopt,
        ) {
            Ok(p) => p,
            Err(reason) => {
                skipped.push(blocked(
                    &cand.entry,
                    CheckStatus::Conflict.as_str(),
                    format!("installation aborted: {reason}"),
                ));
                exit = exit.combine(Exit::Blocked);
                continue;
            }
        };
        let touched: Vec<String> = extracted
            .folders
            .iter()
            .chain(plan.move_aside().iter())
            .map(|f| f.to_lowercase())
            .collect();
        if let Some(dup) = touched.iter().find(|f| claimed.contains(*f)) {
            skipped.push(blocked(
                &cand.entry,
                CheckStatus::Conflict.as_str(),
                format!("installation aborted: folder \"{dup}\" is also shipped by another addon in this run"),
            ));
            exit = exit.combine(Exit::Blocked);
            continue;
        }
        claimed.extend(touched);
        warnings.extend(plan.warnings.clone());
        ready.push(Ready {
            cand,
            extracted,
            plan,
            warnings,
        });
    }

    if ready.is_empty() {
        let mut text = report_skipped(&skipped);
        text.push("Nothing to install.".into());
        ctx.out.print(&text.join("\n"));
        return Ok(Outcome::with_exit(
            exit,
            json!({ "profile": profile.name, "dry_run": args.dry_run, "installed": [], "skipped": skipped }),
        ));
    }

    if settings.backup_enabled {
        let backup_size: u64 = ready
            .iter()
            .flat_map(|r| r.plan.move_aside())
            .map(|f| dir_size(&profile.addons_path.join(f)))
            .sum();
        ensure_space(&txc.backups_dir(), backup_size, "backups")?;
    }

    // --- Plan and confirmation ----------------------------------------------
    let plan_json: Vec<_> = ready.iter().map(plan_entry_json).collect();
    let mut text = vec![
        format!(
            "Profile: {}  (client: {}, version type: {}, channel: {})",
            profile.name,
            resolved.client.display(),
            resolved.version_type_id,
            resolved.channel
        ),
        if args.dry_run {
            "Plan (dry run):".to_string()
        } else {
            format!("Plan (transaction {tx_id}):")
        },
    ];
    for r in &ready {
        text.extend(plan_lines(r));
    }
    text.extend(report_skipped(&skipped));
    ctx.out.print(&text.join("\n"));
    if wow_running() {
        ctx.out.warn(
            "World of Warcraft seems to be running; changes take effect after /reload or a restart",
        );
    }

    if args.dry_run {
        ctx.out.print("Dry run: nothing was changed.");
        return Ok(Outcome::with_exit(
            exit,
            json!({ "profile": profile.name, "dry_run": true, "plan": plan_json, "skipped": skipped }),
        ));
    }
    let question = format!("Install {}?", plural(ready.len(), "addon"));
    if !ctx
        .out
        .confirm(&question, args.yes || settings.auto_confirm)?
    {
        return Err(AppError::Cancelled);
    }

    // --- Transaction ----------------------------------------------------------
    let now = OffsetDateTime::now_utc();
    let items: Vec<JournalItem> = ready
        .iter()
        .map(|r| {
            let entry = &r.cand.entry;
            let mut folders = r.extracted.folders.clone();
            let main = folders
                .iter()
                .find(|f| f.eq_ignore_ascii_case(&entry.name))
                .cloned()
                .or_else(|| folders.iter().min_by_key(|f| f.len()).cloned())
                .unwrap_or_default();
            folders.sort_by_key(|f| (*f != main, f.to_lowercase()));
            JournalItem {
                project: entry.project.clone(),
                addon: entry.name.clone(),
                action: format!("{:?}", r.cand.action).to_uppercase(),
                display_from: entry
                    .managed
                    .as_ref()
                    .map(|m| m.display_version.clone())
                    .or(entry.local_version.clone()),
                display_to: Some(r.cand.file.display_name.clone()),
                staging: r.extracted.root.clone(),
                move_aside: r.plan.move_aside(),
                removed: r.plan.remove.clone(),
                install: r.extracted.folders.clone(),
                previous: entry.managed.clone(),
                next: Some(ManagedProject {
                    file_id: Some(r.cand.file.id.clone()),
                    archive_sha1: None,
                    display_version: r.cand.file.display_name.clone(),
                    file_date: r.cand.file.published_at,
                    folders,
                    client: profile.client,
                    game_version_type_id: Some(resolved.version_type_id),
                    client_mismatch: !resolver::compatibility::matches_client(
                        &r.cand.file,
                        &resolved.criteria(false),
                    ),
                    installed_at: now,
                    transaction_id: tx_id.clone(),
                }),
            }
        })
        .collect();
    let journal = Journal {
        schema_version: installer::journal::JOURNAL_SCHEMA_VERSION,
        id: tx_id.clone(),
        profile: profile.name.to_string(),
        kind: TxKind::Update,
        status: TxStatus::Prepared,
        created_at: now,
        finished_at: None,
        addons_path: profile.addons_path.clone(),
        tx_dir: tx_dir.clone(),
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
        "Installed {} (transaction {}).",
        plural(ready.len(), "addon"),
        outcome.journal.id
    )];
    if let Some(b) = &outcome.journal.backup_dir {
        done.push(format!("Backup: {}", b.display()));
    }
    ctx.out.print(&done.join("\n"));
    Ok(Outcome::with_exit(
        exit,
        json!({
            "profile": profile.name,
            "dry_run": false,
            "transaction_id": outcome.journal.id,
            "installed": plan_json,
            "skipped": skipped,
            "backup_dir": outcome.journal.backup_dir,
            "warnings": outcome.warnings,
        }),
    ))
}

/// Turn an evaluation into a candidate, or record why it is skipped.
async fn pick(
    provider: &dyn AddonProvider,
    ev: Evaluation,
    criteria: &Criteria,
    args: &UpdateArgs,
    named: bool,
    skipped: &mut Vec<Skipped>,
    exit: &mut Exit,
) -> Result<Option<Candidate>> {
    let entry = ev.entry.clone();
    let file = ev.available.clone();
    match ev.status {
        CheckStatus::UpdateAvailable => Ok(file.map(|file| Candidate {
            entry,
            file,
            action: Action::Update,
        })),
        CheckStatus::Unmanaged => Ok(file.map(|file| Candidate {
            entry,
            file,
            action: Action::Adopt,
        })),
        CheckStatus::UpToDate | CheckStatus::Missing if args.reinstall => {
            let Some(managed) = entry.managed.clone() else {
                return Ok(None);
            };
            let Some(file_id) = managed.file_id.clone() else {
                skipped.push(blocked(
                    &entry,
                    "ERROR",
                    "cannot reinstall: it was imported from a ZIP file;                      put that file in the inbox again and run `wowaddons import`",
                ));
                *exit = exit.combine(Exit::Partial);
                return Ok(None);
            };
            let project = entry
                .project
                .clone()
                .expect("managed entries have a project");
            match provider.get_file(&project, &file_id).await {
                Ok(f) => {
                    let mut c = criteria.clone();
                    c.allow_client_mismatch |= managed.client_mismatch;
                    c.channel = crate::domain::ReleaseChannel::Alpha;
                    match resolver::eligibility(&f, &c) {
                        Ok(()) => Ok(Some(Candidate {
                            entry,
                            file: f,
                            action: Action::Reinstall,
                        })),
                        Err(why) => {
                            skipped.push(blocked(
                                &entry,
                                "ERROR",
                                format!("cannot reinstall: {}", why.describe()),
                            ));
                            *exit = exit.combine(Exit::Partial);
                            Ok(None)
                        }
                    }
                }
                Err(e) if e.is_fatal() => Err(e.into()),
                Err(e) => {
                    skipped.push(blocked(&entry, "ERROR", format!("cannot reinstall: {e}")));
                    *exit = exit.combine(Exit::Partial);
                    Ok(None)
                }
            }
        }
        CheckStatus::UpToDate => Ok(None),
        CheckStatus::NoCompatibleRelease | CheckStatus::DownloadNotAllowed => {
            skipped.push(blocked(
                &entry,
                ev.status.as_str(),
                ev.message
                    .clone()
                    .unwrap_or_else(|| "no compatible release for this client".into()),
            ));
            if named {
                *exit = exit.combine(Exit::Blocked);
            }
            Ok(None)
        }
        CheckStatus::Error => {
            skipped.push(blocked(
                &entry,
                "ERROR",
                ev.message.clone().unwrap_or_default(),
            ));
            *exit = exit.combine(Exit::Partial);
            Ok(None)
        }
        other => {
            if named {
                skipped.push(blocked(
                    &entry,
                    other.as_str(),
                    "this addon cannot be updated",
                ));
                *exit = exit.combine(Exit::Blocked);
            }
            Ok(None)
        }
    }
}

/// `--file-id`: install one exact file after the usual checks.
async fn explicit_candidate(
    provider: &dyn AddonProvider,
    entry: AddonEntry,
    file_id: &str,
    criteria: &Criteria,
    args: &UpdateArgs,
) -> Result<Candidate> {
    let project = entry
        .project
        .clone()
        .ok_or_else(|| AppError::Blocked(format!("{} is not linked to a project", entry.name)))?;
    let projects = provider
        .get_projects(std::slice::from_ref(&project))
        .await?;
    let info = projects
        .into_iter()
        .find(|p| p.project == project)
        .ok_or_else(|| AppError::Remote(format!("unknown project {project}")))?;
    if !info.is_wow {
        return Err(AppError::Blocked(format!(
            "{project} is not a World of Warcraft project"
        )));
    }
    if info.allow_distribution == Some(false) {
        return Err(AppError::Blocked(
            "the author does not allow third-party downloads".into(),
        ));
    }
    let file = match provider.get_file(&project, file_id).await {
        Ok(f) => f,
        Err(ProviderError::NotFound(_)) => {
            return Err(AppError::Usage(format!(
                "file {file_id} does not exist for {project}"
            )));
        }
        Err(e) => return Err(e.into()),
    };
    resolver::eligibility(&file, criteria).map_err(|why| {
        AppError::Blocked(format!(
            "file {file_id} cannot be installed: {}",
            why.describe()
        ))
    })?;
    let action = match (&entry.managed, entry.tracking) {
        (Some(_), Tracking::Missing) => Action::Reinstall,
        (Some(m), _) => match resolver::compare(m, &file) {
            Decision::Newer => Action::Update,
            Decision::Older => Action::Downgrade,
            Decision::Same if args.reinstall => Action::Reinstall,
            Decision::Same => {
                return Err(AppError::Blocked(format!(
                    "file {file_id} is already installed; use --reinstall to repair it"
                )));
            }
        },
        (None, _) if args.adopt => Action::Adopt,
        (None, _) => {
            return Err(AppError::Blocked(format!(
                "{} is not installed by wowaddons; add --adopt",
                entry.name
            )));
        }
    };
    Ok(Candidate {
        entry,
        file,
        action,
    })
}

fn plan_entry_json(r: &Ready) -> serde_json::Value {
    json!({
        "addon": r.cand.entry.name,
        "project": r.cand.entry.project,
        "action": r.cand.action,
        "from": r.cand.entry.managed.as_ref().map(|m| m.display_version.clone()).or(r.cand.entry.local_version.clone()),
        "to": r.cand.file.display_name,
        "file_id": r.cand.file.id,
        "add": r.plan.add,
        "replace": r.plan.replace.iter().map(|(old, _)| old).collect::<Vec<_>>(),
        "remove": r.plan.remove,
        "warnings": r.warnings,
    })
}

fn plan_lines(r: &Ready) -> Vec<String> {
    let action = match r.cand.action {
        Action::Update => "UPDATE",
        Action::Adopt => "ADOPT",
        Action::Reinstall => "REINSTALL",
        Action::Downgrade => "DOWNGRADE",
    };
    let from = r
        .cand
        .entry
        .managed
        .as_ref()
        .map(|m| m.display_version.clone())
        .or(r.cand.entry.local_version.clone())
        .unwrap_or_else(|| "-".into());
    let mut lines = vec![format!(
        "  {action:<10} {}  {from} -> {}  (file {})",
        r.cand.entry.name, r.cand.file.display_name, r.cand.file.id
    )];
    let replace: Vec<&str> = r.plan.replace.iter().map(|(old, _)| old.as_str()).collect();
    if !replace.is_empty() {
        lines.push(format!("             replace: {}", replace.join(", ")));
    }
    if !r.plan.add.is_empty() {
        lines.push(format!("             add: {}", r.plan.add.join(", ")));
    }
    if !r.plan.remove.is_empty() {
        lines.push(format!("             remove: {}", r.plan.remove.join(", ")));
    }
    if r.cand.action == Action::Downgrade {
        lines.push("             warning: this installs an older file (downgrade)".into());
    }
    for w in &r.warnings {
        lines.push(format!("             warning: {w}"));
    }
    lines
}

pub(super) fn report_skipped(skipped: &[Skipped]) -> Vec<String> {
    skipped
        .iter()
        .map(|s| format!("  {:<10} {}: {}", s.status, s.addon, s.message))
        .collect()
}

fn nothing_message(unmanaged_left: usize) -> String {
    if unmanaged_left > 0 {
        format!(
            "Nothing to update. {} can be adopted with `wowaddons update --adopt`.",
            plural(unmanaged_left, "unmanaged addon")
        )
    } else {
        "Nothing to update.".into()
    }
}

/// Used by `rollback` to check folders before reverting.
pub fn state_matches(state: &State, item: &JournalItem) -> bool {
    let current = item.project.as_ref().and_then(|p| state.projects.get(p));
    match (&item.next, current) {
        (Some(next), Some(current)) => {
            next.file_id == current.file_id && next.transaction_id == current.transaction_id
        }
        (None, None) => true,
        _ => false,
    }
}
