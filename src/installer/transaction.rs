//! Transactional install, automatic rollback, post-commit steps and crash recovery.
//! All operations are folder renames on one volume.

use std::path::{Path, PathBuf};

use time::OffsetDateTime;

use crate::cancel;
use crate::domain::ProfileName;
use crate::error::{AppError, Result};
use crate::scanner::is_addon_folder;
use crate::storage::fsutil::{atomic_write, move_dir, remove_dir_if_exists, rename_with_retry};
use crate::storage::{AppPaths, State};

use super::backup::apply_retention;
use super::journal::{Journal, Step, TxStatus, list_journals};

/// Everything a transaction needs to know about its profile.
#[derive(Clone, Debug)]
pub struct TxContext {
    pub paths: AppPaths,
    pub profile: ProfileName,
    pub addons_path: PathBuf,
    pub work_dir: PathBuf,
    pub backup_enabled: bool,
    pub backup_retention: u32,
}

impl TxContext {
    pub fn state_path(&self) -> PathBuf {
        self.paths.state_file(&self.profile)
    }

    pub fn journal_dir(&self) -> PathBuf {
        self.paths.journal_dir(&self.profile)
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.paths.backups_dir(&self.profile)
    }

    pub fn tx_dir(&self, id: &str) -> PathBuf {
        self.work_dir.join(id)
    }

    pub fn journal_path(&self, id: &str) -> PathBuf {
        self.journal_dir().join(format!("{id}.json"))
    }
}

/// Test hooks for fault injection. Production code uses the default.
#[derive(Clone, Debug, Default)]
pub struct TxHooks {
    /// Fail once this many steps have run (0 = right after PREPARED).
    pub fail_after_steps: Option<usize>,
    /// Fail after all steps, just before the commit.
    pub fail_before_commit: bool,
    /// Simulate a killed process: stop without rolling back.
    pub crash: bool,
}

/// Result of a committed transaction.
#[derive(Debug)]
pub struct TxOutcome {
    pub journal: Journal,
    /// Post-commit problems (the commit itself succeeded).
    pub warnings: Vec<String>,
}

enum Failure {
    Error(String),
    Cancelled,
    Crash,
}

/// Check that the work dir and `AddOns` are on the same volume with a test rename.
pub fn probe_same_volume(work_dir: &Path, addons_path: &Path, id: &str) -> Result<()> {
    std::fs::create_dir_all(work_dir)
        .map_err(|e| AppError::io("cannot create the work dir", work_dir, e))?;
    let probe = work_dir.join(format!(".probe-{id}"));
    std::fs::write(&probe, b"")
        .map_err(|e| AppError::io("cannot write to the work dir", work_dir, e))?;
    let target = addons_path.join(format!(".wowaddons-probe-{id}"));
    let result = std::fs::rename(&probe, &target);
    let _ = std::fs::remove_file(&probe);
    match result {
        Ok(()) => {
            let _ = std::fs::remove_file(&target);
            Ok(())
        }
        Err(e) => Err(AppError::Blocked(format!(
            "the work dir {} cannot be renamed into {} ({e}); it must be on the same volume. \
             Set another one with `wowaddons profile set <profile> work_dir <path>`",
            work_dir.display(),
            addons_path.display()
        ))),
    }
}

/// Run a prepared journal: move folders aside, install new ones, write state, commit.
/// On failure, everything is restored (`InstallFailed`), or `RollbackFailed` if that fails too.
pub fn execute(ctx: &TxContext, mut journal: Journal, hooks: &TxHooks) -> Result<TxOutcome> {
    journal.path = ctx.journal_path(&journal.id);
    std::fs::create_dir_all(&journal.tx_dir)
        .map_err(|e| AppError::io("cannot create the work dir", &journal.tx_dir, e))?;
    let state_path = ctx.state_path();
    journal.had_state = state_path.exists();
    if journal.had_state {
        std::fs::copy(&state_path, journal.state_backup())
            .map_err(|e| AppError::io("cannot back up the state", &state_path, e))?;
    }
    journal.status = TxStatus::Prepared;
    journal.save()?;

    match run_steps(ctx, &mut journal, hooks) {
        Ok(()) => {
            journal.status = TxStatus::Committed;
            journal.save()?;
            let warnings = finalize(ctx, &mut journal);
            Ok(TxOutcome { journal, warnings })
        }
        Err(Failure::Crash) => Err(AppError::Other("simulated crash".into())),
        Err(failure) => {
            let (reason, cancelled) = match failure {
                Failure::Error(m) => (m, false),
                _ => ("cancelled by the user".to_string(), true),
            };
            journal.error = Some(reason.clone());
            match rollback_journal(ctx, &mut journal) {
                Ok(()) => {
                    if cancelled {
                        Err(AppError::Cancelled)
                    } else {
                        Err(AppError::InstallFailed(format!(
                            "update failed: {reason}. Previous installation restored."
                        )))
                    }
                }
                Err(e) => Err(e),
            }
        }
    }
}

fn run_steps(
    ctx: &TxContext,
    j: &mut Journal,
    hooks: &TxHooks,
) -> std::result::Result<(), Failure> {
    let check = |j: &Journal| -> std::result::Result<(), Failure> {
        if hooks.fail_after_steps == Some(j.steps.len()) {
            return Err(if hooks.crash {
                Failure::Crash
            } else {
                Failure::Error("injected failure".into())
            });
        }
        if cancel::is_cancelled() {
            return Err(Failure::Cancelled);
        }
        Ok(())
    };
    let io = |what: &str, p: &Path, e: std::io::Error| {
        Failure::Error(format!("{what} {}: {e}", p.display()))
    };
    let save = |j: &Journal| j.save().map_err(|e| Failure::Error(e.to_string()));

    check(j)?;
    let previous = j.previous_dir();
    std::fs::create_dir_all(&previous).map_err(|e| io("cannot create", &previous, e))?;

    // 1. Move existing folders aside.
    let moves: Vec<String> = j.items.iter().flat_map(|i| i.move_aside.clone()).collect();
    for folder in moves {
        j.steps.push(Step::MoveAside {
            folder: folder.clone(),
        });
        save(j)?;
        let from = ctx.addons_path.join(&folder);
        rename_with_retry(&from, &previous.join(&folder))
            .map_err(|e| io("cannot move", &from, e))?;
        check(j)?;
    }

    // 2. Install new folders.
    for idx in 0..j.items.len() {
        for folder in j.items[idx].install.clone() {
            j.steps.push(Step::Install {
                item: idx,
                folder: folder.clone(),
            });
            save(j)?;
            let from = j.items[idx].staging.join(&folder);
            rename_with_retry(&from, &ctx.addons_path.join(&folder))
                .map_err(|e| io("cannot install", &from, e))?;
            check(j)?;
        }
    }

    // 3. Check the installed content.
    for item in &j.items {
        for folder in &item.install {
            let path = ctx.addons_path.join(folder);
            if !is_addon_folder(folder, &path) {
                return Err(Failure::Error(format!(
                    "installed folder {} has no matching .toc file",
                    path.display()
                )));
            }
        }
    }

    // 4. New state.
    let state_path = ctx.state_path();
    let mut state = State::load(&state_path, ctx.profile.as_str())
        .map_err(|e| Failure::Error(e.to_string()))?;
    for item in &j.items {
        let Some(project) = &item.project else {
            continue;
        };
        state.projects.remove(project);
        if let Some(next) = &item.next {
            state.projects.insert(project.clone(), next.clone());
        }
    }
    if hooks.fail_before_commit {
        return Err(Failure::Error("injected failure before commit".into()));
    }
    state
        .save(&state_path)
        .map_err(|e| Failure::Error(e.to_string()))?;
    Ok(())
}

/// Undo the journaled steps, checking the filesystem for each one, then restore the state.
pub fn rollback_journal(ctx: &TxContext, j: &mut Journal) -> Result<()> {
    let mut problems = Vec::new();
    let previous = j.previous_dir();
    for step in j.steps.clone().iter().rev() {
        match step {
            Step::Install { item, folder } => {
                let staged = j.items.get(*item).map(|i| i.staging.join(folder));
                let target = ctx.addons_path.join(folder);
                let installed = staged.as_ref().is_some_and(|s| !s.exists()) && target.exists();
                if installed && let Err(e) = std::fs::remove_dir_all(&target) {
                    problems.push(format!("cannot remove {}: {e}", target.display()));
                }
            }
            Step::MoveAside { folder } => {
                let saved = previous.join(folder);
                let target = ctx.addons_path.join(folder);
                if saved.exists() {
                    if target.exists() {
                        problems.push(format!(
                            "cannot restore {}: a folder with that name is in the way",
                            target.display()
                        ));
                    } else if let Err(e) = rename_with_retry(&saved, &target) {
                        problems.push(format!("cannot restore {}: {e}", target.display()));
                    }
                }
            }
        }
    }

    let state_path = ctx.state_path();
    if j.had_state {
        match std::fs::read(j.state_backup()) {
            Ok(bytes) => {
                if let Err(e) = atomic_write(&state_path, &bytes) {
                    problems.push(format!("cannot restore the state: {e}"));
                }
            }
            Err(e) => problems.push(format!("cannot read the state backup: {e}")),
        }
    } else if state_path.exists()
        && let Err(e) = std::fs::remove_file(&state_path)
    {
        problems.push(format!("cannot restore the state: {e}"));
    }

    j.finished_at = Some(OffsetDateTime::now_utc());
    if problems.is_empty() {
        j.status = TxStatus::RolledBack;
        j.save()?;
        let _ = remove_dir_if_exists(&j.tx_dir);
        Ok(())
    } else {
        j.status = TxStatus::RollbackFailed;
        let detail = problems.join("; ");
        j.error = Some(match &j.error {
            Some(e) => format!("{e}; rollback failed: {detail}"),
            None => format!("rollback failed: {detail}"),
        });
        j.save()?;
        Err(AppError::RollbackFailed(format!(
            "rollback of transaction {} failed: {detail}. Nothing else was touched. \
             Previous folders are in {}; the journal is {}",
            j.id,
            previous.display(),
            j.path.display()
        )))
    }
}

/// Post-commit steps. Failures are returned as warnings; the journal stays COMMITTED
/// so recovery can finish them later.
pub fn finalize(ctx: &TxContext, j: &mut Journal) -> Vec<String> {
    let mut warnings = Vec::new();
    let previous = j.previous_dir();
    if ctx.backup_enabled {
        let dest = ctx.backups_dir().join(&j.id);
        let folders = dest.join("folders");
        let result = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(&dest)?;
            if previous.exists() {
                move_dir(&previous, &folders)?;
            } else {
                std::fs::create_dir_all(&folders)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                j.backup_dir = Some(dest.clone());
                if let Ok(json) = serde_json::to_vec_pretty(&*j)
                    && let Err(e) = atomic_write(&dest.join("manifest.json"), &json)
                {
                    warnings.push(format!("cannot write the backup manifest: {e}"));
                }
            }
            Err(e) => {
                warnings.push(format!("cannot move the backup to {}: {e}", dest.display()));
                let _ = j.save();
                return warnings;
            }
        }
        warnings.extend(apply_retention(&ctx.backups_dir(), ctx.backup_retention));
    }
    if let Err(e) = remove_dir_if_exists(&j.tx_dir) {
        warnings.push(format!(
            "cannot clean the work dir {}: {e}",
            j.tx_dir.display()
        ));
    }
    j.status = TxStatus::Finalized;
    j.finished_at = Some(OffsetDateTime::now_utc());
    if let Err(e) = j.save() {
        warnings.push(e.to_string());
    }
    warnings
}

/// Finish or undo interrupted transactions of a profile.
/// Call with the profile lock held. Returns messages to show to the user.
pub fn recover(ctx: &TxContext) -> Result<Vec<String>> {
    let mut messages = Vec::new();
    let journals = list_journals(&ctx.journal_dir())?;
    for mut j in journals.clone() {
        match j.status {
            TxStatus::Prepared => {
                j.error.get_or_insert_with(|| "interrupted".into());
                rollback_journal(ctx, &mut j)?;
                messages.push(format!(
                    "recovered interrupted transaction {}: previous installation restored",
                    j.id
                ));
            }
            TxStatus::Committed => {
                let warnings = finalize(ctx, &mut j);
                messages.push(format!(
                    "finished post-commit steps of transaction {}",
                    j.id
                ));
                messages.extend(warnings);
            }
            TxStatus::RollbackFailed => {
                return Err(AppError::RollbackFailed(format!(
                    "transaction {} could not be rolled back and needs manual repair ({}). \
                     Journal: {}",
                    j.id,
                    j.error.as_deref().unwrap_or("unknown error"),
                    j.path.display()
                )));
            }
            TxStatus::Finalized | TxStatus::RolledBack => {}
        }
    }
    // Work dirs left behind by a run that stopped before writing its journal.
    if let Ok(entries) = std::fs::read_dir(&ctx.work_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let active = journals.iter().any(|j| {
                j.id == name
                    && matches!(
                        j.status,
                        TxStatus::Prepared | TxStatus::Committed | TxStatus::RollbackFailed
                    )
            });
            if !active && e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let _ = remove_dir_if_exists(&e.path());
            }
        }
    }
    Ok(messages)
}

/// Journals that need attention (for `doctor`), without changing anything.
pub fn pending(ctx: &TxContext) -> Result<Vec<Journal>> {
    Ok(list_journals(&ctx.journal_dir())?
        .into_iter()
        .filter(|j| {
            matches!(
                j.status,
                TxStatus::Prepared | TxStatus::Committed | TxStatus::RollbackFailed
            )
        })
        .collect())
}
