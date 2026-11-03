//! `wowaddons inbox` and `wowaddons import`: install ZIP files downloaded by hand,
//! for projects whose authors block third-party downloads. Both work offline.
//!
//! An imported archive goes through the same checks and transaction as `update`.
//! Its remote file ID is unknown, so the state records the archive's SHA-1 instead;
//! `check` uses it later to find the exact remote file.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::json;
use time::OffsetDateTime;

use crate::cli::{ImportArgs, InboxArgs};
use crate::domain::{ProjectRef, Tracking};
use crate::error::{AppError, Exit, Result};
use crate::installer::{
    self, ArchiveLimits, ExtractedArchive, FolderPlan, Journal, JournalItem, TxHooks, TxKind,
    TxStatus, hash_file, new_transaction_id, plan_folders, probe_same_volume,
};
use crate::output::{Output, plural};
use crate::scanner::{self, AddonEntry, Inventory, build_entries};
use crate::storage::fsutil::{dir_size, ensure_space};
use crate::storage::{FileLock, ManagedProject, State};

use super::update::TxDirGuard;
use super::{Ctx, Outcome, wow_running};

/// An archive that passed every check and is ready to install.
struct Item {
    archive: PathBuf,
    archive_name: String,
    sha1: String,
    entry: AddonEntry,
    project: ProjectRef,
    version: String,
    extracted: ExtractedArchive,
    plan: FolderPlan,
    warnings: Vec<String>,
}

/// An archive that is not installed, and why.
#[derive(Debug, Serialize)]
struct Skipped {
    archive: String,
    addon: Option<String>,
    status: &'static str,
    message: String,
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// ZIP files directly in the inbox, sorted by name. Links and folders are ignored.
fn inbox_archives(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(AppError::io("cannot read the inbox", dir, e)),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        let is_zip = path
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("zip"));
        if is_file && is_zip {
            out.push(path);
        }
    }
    out.sort_by_key(|p| file_name(p).to_lowercase());
    Ok(out)
}

/// `wowaddons inbox`: create the profile's inbox and open it in the file manager.
pub fn inbox(ctx: &Ctx, args: &InboxArgs) -> Result<Outcome> {
    let config = ctx.load_config()?;
    let profile = ctx.profile(&config)?;
    let dir = ctx.paths.inbox_dir(&profile.name);
    std::fs::create_dir_all(&dir).map_err(|e| AppError::io("cannot create the inbox", &dir, e))?;
    let names: Vec<String> = inbox_archives(&dir)?.iter().map(|p| file_name(p)).collect();

    let mut text = vec![format!(
        "Inbox of profile \"{}\": {}",
        profile.name,
        dir.display()
    )];
    if names.is_empty() {
        text.push(
            "It is empty. Save the ZIP files you download by hand here, \
             then run `wowaddons import`."
                .into(),
        );
    } else {
        text.push(format!(
            "{} waiting: {}",
            plural(names.len(), "ZIP file"),
            names.join(", ")
        ));
        text.push("Run `wowaddons import` to install them.".into());
    }
    ctx.out.print(&text.join("\n"));
    let opened = !args.no_open && open_folder(&dir, ctx.out);
    Ok(Outcome::ok(json!({
        "profile": profile.name,
        "path": dir,
        "archives": names,
        "opened": opened,
    })))
}

/// Open a folder in the system file manager without waiting for it.
fn open_folder(dir: &Path, out: &Output) -> bool {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let result = Command::new(program)
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match result {
        Ok(_) => true,
        Err(e) => {
            out.warn(&format!(
                "cannot open the folder with {program} ({e}); open it by hand"
            ));
            false
        }
    }
}

/// Which project an extracted archive belongs to.
///
/// The archive's own TOC files come first. Without them, the installed folders with
/// the same names tell (state, explicit links, then their TOC files).
fn identify(
    folders: &[String],
    content: &Inventory,
    inventory: &Inventory,
    state: &State,
) -> std::result::Result<ProjectRef, (&'static str, String)> {
    let mut declared: BTreeSet<ProjectRef> = BTreeSet::new();
    for folder in folders {
        if let Some(a) = content.find(folder) {
            declared.extend(a.declared_projects.iter().cloned());
        }
    }
    if declared.is_empty() {
        for folder in folders {
            if let Some((p, _)) = state.owner_of(folder) {
                declared.insert(p.clone());
            } else if let Some((_, p)) = state.link_for(folder) {
                declared.insert(p.clone());
            } else if let Some(a) = inventory.find(folder)
                && !a.has_project_conflict()
            {
                declared.extend(a.declared_projects.first().cloned());
            }
        }
    }
    let mut it = declared.iter();
    match (it.next(), it.next()) {
        (Some(p), None) => Ok(p.clone()),
        (None, _) => Err((
            "UNTRACKED",
            "cannot tell which CurseForge project this archive belongs to \
             (no X-Curse-Project-ID in its TOC files, and no installed addon with the same folders); \
             link the installed addon with `wowaddons link <addon> --project-id <id>`"
                .into(),
        )),
        _ => Err((
            "CONFLICT",
            format!(
                "the archive matches several projects ({})",
                declared
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

/// The installed addon of a project: the managed one first, then an unmanaged one.
fn installed_entry<'a>(entries: &'a [AddonEntry], project: &ProjectRef) -> Option<&'a AddonEntry> {
    let of_project = |e: &&AddonEntry| e.project.as_ref() == Some(project);
    entries
        .iter()
        .filter(of_project)
        .find(|e| matches!(e.tracking, Tracking::Managed | Tracking::Missing))
        .or_else(|| {
            entries
                .iter()
                .filter(of_project)
                .find(|e| e.tracking == Tracking::Unmanaged)
        })
}

/// Main folder first: the one named like the addon, else the shortest name.
fn ordered_folders(folders: &[String], addon: &str) -> Vec<String> {
    let main = folders
        .iter()
        .find(|f| f.eq_ignore_ascii_case(addon))
        .or_else(|| folders.iter().min_by_key(|f| (f.len(), f.to_lowercase())))
        .cloned()
        .unwrap_or_default();
    let mut out = folders.to_vec();
    out.sort_by_key(|f| (*f != main, f.to_lowercase()));
    out
}

/// `wowaddons import`: install the ZIP files of the inbox in one transaction.
pub fn import(ctx: &Ctx, args: &ImportArgs) -> Result<Outcome> {
    let config = ctx.load_config()?;
    let settings = config.settings();
    let profile = ctx.profile(&config)?;
    let txc = ctx.tx_context(&profile, &settings);
    let _lock = FileLock::acquire(&ctx.paths.lock_file(&profile.name), profile.name.as_str())?;
    for msg in installer::recover(&txc)? {
        ctx.out.warn(&msg);
    }

    let inbox = ctx.paths.inbox_dir(&profile.name);
    let archives = inbox_archives(&inbox)?;
    let empty_result = |skipped: &[Skipped], exit: Exit| {
        Outcome::with_exit(
            exit,
            json!({
                "profile": profile.name,
                "inbox": inbox,
                "dry_run": args.dry_run,
                "installed": [],
                "skipped": skipped,
            }),
        )
    };
    if archives.is_empty() {
        ctx.out.print(&format!(
            "The inbox is empty ({}).\n\
             Run `wowaddons inbox` to open it, save the ZIP files there, then run `wowaddons import` again.",
            inbox.display()
        ));
        return Ok(empty_result(&[], Exit::Success));
    }

    let suffixes = profile.toc_suffixes();
    let state = ctx.load_state(&profile)?;
    let inventory = scanner::scan(&profile.addons_path, &suffixes)?;
    let entries = build_entries(&inventory, &state);

    let tx_id = new_transaction_id();
    let tx_dir = txc.tx_dir(&tx_id);
    let mut guard = TxDirGuard(Some(tx_dir.clone()));
    probe_same_volume(&txc.work_dir, &profile.addons_path, &tx_id)?;
    let expected: u64 = archives
        .iter()
        .map(|p| {
            std::fs::metadata(p)
                .map(|m| m.len())
                .unwrap_or(0)
                .saturating_mul(3)
        })
        .sum();
    ensure_space(&txc.work_dir, expected, "extraction")?;

    // --- Check, extract and identify every archive (no change to AddOns) -----
    let limits = ArchiveLimits {
        max_entries: settings.max_archive_entries,
        max_extracted_bytes: settings.max_extracted_size_mb * 1024 * 1024,
    };
    let max_bytes = settings.max_archive_size_mb * 1024 * 1024;
    let mut skipped: Vec<Skipped> = Vec::new();
    let mut exit = Exit::Success;
    // Archives identical to what is installed: removed from the inbox like imported ones.
    let mut already_installed: Vec<PathBuf> = Vec::new();
    let mut items: Vec<Item> = Vec::new();
    for (i, path) in archives.iter().enumerate() {
        let archive_name = file_name(path);
        let mut skip = |addon: Option<&str>, status: &'static str, message: String, e: Exit| {
            skipped.push(Skipped {
                archive: archive_name.clone(),
                addon: addon.map(str::to_string),
                status,
                message,
            });
            exit = exit.combine(e);
        };
        let (size, sha1) = match hash_file(path) {
            Ok(x) => x,
            Err(e) => {
                skip(
                    None,
                    "ERROR",
                    format!("cannot read the file: {e}"),
                    Exit::Partial,
                );
                continue;
            }
        };
        if size > max_bytes {
            skip(
                None,
                "ERROR",
                format!(
                    "the file is larger than the limit ({} MB)",
                    settings.max_archive_size_mb
                ),
                Exit::Partial,
            );
            continue;
        }
        let staging = tx_dir.join("staging").join(i.to_string());
        let extracted = match installer::extract(path, &staging, limits) {
            Ok(x) => x,
            Err(e) => {
                skip(
                    None,
                    "ERROR",
                    format!("archive rejected: {e}"),
                    Exit::Partial,
                );
                continue;
            }
        };
        let content = match scanner::scan(&extracted.root, &suffixes) {
            Ok(c) => c,
            Err(e) => {
                skip(
                    None,
                    "ERROR",
                    format!("cannot read the archive: {e}"),
                    Exit::Partial,
                );
                continue;
            }
        };
        let project = match identify(&extracted.folders, &content, &inventory, &state) {
            Ok(p) => p,
            Err((status, message)) => {
                skip(None, status, message, Exit::Blocked);
                continue;
            }
        };
        let Some(entry) = installed_entry(&entries, &project).cloned() else {
            skip(
                None,
                "NOT_INSTALLED",
                format!(
                    "{project} is not installed in profile \"{}\"; \
                     wowaddons only updates addons that are already there",
                    profile.name
                ),
                Exit::Blocked,
            );
            continue;
        };
        let installed_sha1 = entry
            .managed
            .as_ref()
            .and_then(|m| m.archive_sha1.as_deref());
        if installed_sha1 == Some(sha1.as_str()) && entry.tracking == Tracking::Managed {
            skip(
                Some(&entry.name),
                "ALREADY_INSTALLED",
                "this exact file is already installed".into(),
                Exit::Success,
            );
            already_installed.push(path.clone());
            continue;
        }
        let folders = ordered_folders(&extracted.folders, &entry.name);
        let version = content
            .find(&folders[0])
            .and_then(|a| a.toc_version.clone())
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
        let plan = match plan_folders(
            &project,
            &extracted.folders,
            &profile.addons_path,
            &inventory,
            &state,
            entry.managed.is_none(),
        ) {
            Ok(p) => p,
            Err(reason) => {
                skip(
                    Some(&entry.name),
                    "CONFLICT",
                    format!("installation aborted: {reason}"),
                    Exit::Blocked,
                );
                continue;
            }
        };
        let mut warnings: Vec<String> = content
            .addons
            .iter()
            .flat_map(|a| a.warnings.iter().map(move |w| format!("{}: {w}", a.folder)))
            .collect();
        warnings.extend(plan.warnings.clone());
        items.push(Item {
            archive: path.clone(),
            archive_name,
            sha1,
            entry,
            project,
            version,
            extracted,
            plan,
            warnings,
        });
    }

    // Several archives for one project, or one folder shipped twice: block them all.
    let mut per_project: BTreeMap<ProjectRef, Vec<String>> = BTreeMap::new();
    for it in &items {
        per_project
            .entry(it.project.clone())
            .or_default()
            .push(it.archive_name.clone());
    }
    let mut ready: Vec<Item> = Vec::new();
    let mut claimed: HashSet<String> = HashSet::new();
    for it in items {
        let names = &per_project[&it.project];
        if names.len() > 1 {
            skipped.push(Skipped {
                archive: it.archive_name.clone(),
                addon: Some(it.entry.name.clone()),
                status: "DUPLICATE",
                message: format!(
                    "several ZIP files for {} ({}); keep only one in the inbox",
                    it.project,
                    names.join(", ")
                ),
            });
            exit = exit.combine(Exit::Blocked);
            continue;
        }
        let touched: Vec<String> = it
            .extracted
            .folders
            .iter()
            .chain(it.plan.move_aside().iter())
            .map(|f| f.to_lowercase())
            .collect();
        if let Some(dup) = touched.iter().find(|f| claimed.contains(*f)) {
            skipped.push(Skipped {
                archive: it.archive_name.clone(),
                addon: Some(it.entry.name.clone()),
                status: "CONFLICT",
                message: format!(
                    "installation aborted: folder \"{dup}\" is also shipped by another archive"
                ),
            });
            exit = exit.combine(Exit::Blocked);
            continue;
        }
        claimed.extend(touched);
        ready.push(it);
    }

    let skipped_lines: Vec<String> = skipped
        .iter()
        .map(|s| format!("  {:<17} {}: {}", s.status, s.archive, s.message))
        .collect();
    if ready.is_empty() {
        let mut text = skipped_lines;
        text.push("Nothing to install.".into());
        if !args.dry_run {
            remove_archives(&already_installed, ctx.out);
        }
        ctx.out.print(&text.join("\n"));
        return Ok(empty_result(&skipped, exit));
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
        format!("Profile: {}  (inbox: {})", profile.name, inbox.display()),
        if args.dry_run {
            "Plan (dry run):".to_string()
        } else {
            format!("Plan (transaction {tx_id}):")
        },
    ];
    for r in &ready {
        text.extend(plan_lines(r));
    }
    text.extend(skipped_lines);
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
            json!({
                "profile": profile.name,
                "inbox": inbox,
                "dry_run": true,
                "plan": plan_json,
                "skipped": skipped,
            }),
        ));
    }
    let question = format!("Install {} from the inbox?", plural(ready.len(), "addon"));
    if !ctx
        .out
        .confirm(&question, args.yes || settings.auto_confirm)?
    {
        return Err(AppError::Cancelled);
    }

    // --- Transaction ----------------------------------------------------------
    let now = OffsetDateTime::now_utc();
    let journal_items: Vec<JournalItem> = ready
        .iter()
        .map(|r| JournalItem {
            project: r.project.clone(),
            addon: r.entry.name.clone(),
            action: "IMPORT".into(),
            display_from: from_version(&r.entry),
            display_to: Some(r.version.clone()),
            staging: r.extracted.root.clone(),
            move_aside: r.plan.move_aside(),
            removed: r.plan.remove.clone(),
            install: r.extracted.folders.clone(),
            previous: r.entry.managed.clone(),
            next: Some(ManagedProject {
                file_id: None,
                archive_sha1: Some(r.sha1.clone()),
                display_version: r.version.clone(),
                file_date: now,
                folders: ordered_folders(&r.extracted.folders, &r.entry.name),
                client: profile.client,
                game_version_type_id: None,
                client_mismatch: false,
                installed_at: now,
                transaction_id: tx_id.clone(),
            }),
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
        items: journal_items,
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
    let mut imported: Vec<PathBuf> = ready.iter().map(|r| r.archive.clone()).collect();
    imported.extend(already_installed);
    remove_archives(&imported, ctx.out);

    let mut done = vec![format!(
        "Installed {} from the inbox (transaction {}).",
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
            "inbox": inbox,
            "dry_run": false,
            "transaction_id": outcome.journal.id,
            "installed": plan_json,
            "skipped": skipped,
            "backup_dir": outcome.journal.backup_dir,
            "warnings": outcome.warnings,
        }),
    ))
}

/// Remove handled archives from the inbox. A failure only warns: the install is done.
fn remove_archives(paths: &[PathBuf], out: &Output) {
    for p in paths {
        if let Err(e) = std::fs::remove_file(p) {
            out.warn(&format!(
                "installed, but cannot remove {} from the inbox: {e}",
                p.display()
            ));
        }
    }
}

fn from_version(entry: &AddonEntry) -> Option<String> {
    entry
        .managed
        .as_ref()
        .map(|m| m.display_version.clone())
        .or(entry.local_version.clone())
}

fn plan_entry_json(r: &Item) -> serde_json::Value {
    json!({
        "archive": r.archive_name,
        "addon": r.entry.name,
        "project": r.project,
        "action": "IMPORT",
        "adopt": r.entry.managed.is_none(),
        "from": from_version(&r.entry),
        "to": r.version,
        "sha1": r.sha1,
        "add": r.plan.add,
        "replace": r.plan.replace.iter().map(|(old, _)| old).collect::<Vec<_>>(),
        "remove": r.plan.remove,
        "warnings": r.warnings,
    })
}

fn plan_lines(r: &Item) -> Vec<String> {
    let from = from_version(&r.entry).unwrap_or_else(|| "-".into());
    let mut lines = vec![format!(
        "  IMPORT     {}  {from} -> {}  ({})",
        r.entry.name, r.version, r.archive_name
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
    for w in &r.warnings {
        lines.push(format!("             warning: {w}"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::ordered_folders;

    #[test]
    fn main_folder_first() {
        let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            ordered_folders(&f(&["Details_Options", "Details"]), "details"),
            f(&["Details", "Details_Options"])
        );
        assert_eq!(
            ordered_folders(&f(&["B_Long", "Aa"]), "Missing"),
            f(&["Aa", "B_Long"])
        );
    }
}
