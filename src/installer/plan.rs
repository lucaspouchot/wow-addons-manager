//! Folder plan and conflict detection.

use std::path::Path;

use serde::Serialize;

use crate::domain::ProjectRef;
use crate::scanner::Inventory;
use crate::storage::State;

#[derive(Clone, Debug, Default, Serialize)]
pub struct FolderPlan {
    /// New folders.
    pub add: Vec<String>,
    /// (existing name on disk, new name from the archive).
    pub replace: Vec<(String, String)>,
    /// Folders of the managed project that the new archive no longer ships.
    pub remove: Vec<String>,
    pub warnings: Vec<String>,
}

impl FolderPlan {
    /// Folders on disk to move aside (replaced and removed).
    pub fn move_aside(&self) -> Vec<String> {
        self.replace
            .iter()
            .map(|(old, _)| old.clone())
            .chain(self.remove.iter().cloned())
            .collect()
    }
}

/// An existing direct child of `addons_path` matching `name` (case-insensitive).
fn existing_dir(addons_path: &Path, name: &str) -> Option<(String, bool)> {
    let entries = std::fs::read_dir(addons_path).ok()?;
    for e in entries.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.eq_ignore_ascii_case(name) {
            let link = e.file_type().map(|t| t.is_symlink()).unwrap_or(true);
            return Some((n, link));
        }
    }
    None
}

/// Plan the folders for one project. `adopt` is true when the project is not managed yet.
/// Returns `Err(reason)` on a blocking conflict.
pub fn plan_folders(
    project: &ProjectRef,
    archive_folders: &[String],
    addons_path: &Path,
    inventory: &Inventory,
    state: &State,
    adopt: bool,
) -> Result<FolderPlan, String> {
    let mut plan = FolderPlan::default();
    for folder in archive_folders {
        let Some((existing, is_link)) = existing_dir(addons_path, folder) else {
            plan.add.push(folder.clone());
            continue;
        };
        if is_link {
            return Err(format!(
                "folder \"{existing}\" is a symbolic link (protected)"
            ));
        }
        if existing.to_ascii_lowercase().starts_with("blizzard_") {
            return Err(format!(
                "folder \"{existing}\" is a protected Blizzard folder"
            ));
        }
        if let Some((owner, _)) = state.owner_of(&existing) {
            if owner != project {
                return Err(format!(
                    "folder \"{existing}\" is owned by another addon ({owner})"
                ));
            }
            plan.replace.push((existing, folder.clone()));
            continue;
        }
        // Not managed: who claims it?
        let claimed: Option<ProjectRef> = match state.link_for(&existing) {
            Some((_, p)) => Some(p.clone()),
            None => inventory.find(&existing).and_then(|a| {
                if a.has_project_conflict() {
                    None
                } else {
                    a.declared_projects.first().cloned()
                }
            }),
        };
        match claimed {
            Some(p) if p == *project => plan.replace.push((existing, folder.clone())),
            Some(p) => {
                return Err(format!(
                    "folder \"{existing}\" is identified as another project ({p})"
                ));
            }
            None if adopt => {
                plan.warnings.push(format!(
                    "\"{existing}\" is not identified as {project}; it will be replaced (adoption)"
                ));
                plan.replace.push((existing, folder.clone()));
            }
            None => {
                return Err(format!(
                    "folder \"{existing}\" already exists and is not identified as {project}"
                ));
            }
        }
    }

    if let Some(managed) = state.projects.get(project) {
        for old in &managed.folders {
            let still_shipped = archive_folders.iter().any(|f| f.eq_ignore_ascii_case(old));
            if still_shipped {
                continue;
            }
            if let Some((existing, is_link)) = existing_dir(addons_path, old) {
                if is_link {
                    plan.warnings
                        .push(format!("\"{existing}\" is now a symbolic link; left alone"));
                } else {
                    plan.remove.push(existing);
                }
            }
        }
    } else if adopt {
        // Old folders of an unmanaged install that the archive does not ship: left alone.
        for addon in &inventory.addons {
            let same_project = addon.declared_projects.first() == Some(project)
                || state.link_for(&addon.folder).map(|(_, p)| p) == Some(project);
            let shipped = archive_folders
                .iter()
                .any(|f| f.eq_ignore_ascii_case(&addon.folder));
            if same_project && !shipped {
                plan.warnings.push(format!(
                    "\"{}\" is not in the new archive and is left alone",
                    addon.folder
                ));
            }
        }
    }
    Ok(plan)
}
