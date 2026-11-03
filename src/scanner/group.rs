//! Grouping folders into projects and identifying them.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::domain::{LocalAddon, ProjectRef, Tracking};
use crate::storage::{ManagedProject, State};

use super::Inventory;

/// Where a project identification comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IdSource {
    State,
    Link,
    Toc,
}

/// One addon as the user sees it: one or more folders, maybe a project.
#[derive(Clone, Debug, Serialize)]
pub struct AddonEntry {
    /// Main folder name. Used as `<addon>` on the command line.
    pub name: String,
    /// Folders present on disk.
    pub folders: Vec<String>,
    /// Folders recorded in the state but missing on disk.
    pub missing_folders: Vec<String>,
    pub project: Option<ProjectRef>,
    pub id_source: Option<IdSource>,
    pub tracking: Tracking,
    pub title: Option<String>,
    pub local_version: Option<String>,
    #[serde(skip)]
    pub managed: Option<ManagedProject>,
    pub warnings: Vec<String>,
}

impl AddonEntry {
    pub fn matches(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
            || self.folders.iter().any(|f| f.eq_ignore_ascii_case(name))
    }
}

/// Pick the main folder: shortest name, then alphabetical.
fn main_folder(folders: &[String]) -> String {
    folders
        .iter()
        .min_by(|a, b| {
            a.len()
                .cmp(&b.len())
                .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
        })
        .cloned()
        .unwrap_or_default()
}

fn sort_main_first(folders: &mut [String]) {
    let main = main_folder(folders);
    folders.sort_by_key(|f| (!f.eq_ignore_ascii_case(&main), f.to_lowercase()));
}

/// Combine the disk inventory with the profile state.
pub fn build_entries(inv: &Inventory, state: &State) -> Vec<AddonEntry> {
    let mut consumed = vec![false; inv.addons.len()];
    let index_of = |folder: &str| {
        inv.addons
            .iter()
            .position(|a| a.folder.eq_ignore_ascii_case(folder))
    };
    let mut entries = Vec::new();

    // 1. Managed projects from the state.
    for (project, managed) in &state.projects {
        let mut present = Vec::new();
        let mut missing = Vec::new();
        let mut warnings = Vec::new();
        let mut main_addon: Option<&LocalAddon> = None;
        for folder in &managed.folders {
            match index_of(folder) {
                Some(i) if !consumed[i] => {
                    let addon = &inv.addons[i];
                    if addon.protected.is_some() {
                        warnings.push(format!("{} is now a protected folder", addon.folder));
                        missing.push(folder.clone());
                        continue;
                    }
                    consumed[i] = true;
                    present.push(addon.folder.clone());
                    if main_addon.is_none() {
                        main_addon = Some(addon);
                    }
                }
                Some(_) => {
                    warnings.push(format!("{folder} is claimed by several projects"));
                    missing.push(folder.clone());
                }
                None => missing.push(folder.clone()),
            }
        }
        let tracking = if missing.is_empty() {
            Tracking::Managed
        } else {
            Tracking::Missing
        };
        entries.push(AddonEntry {
            name: managed.folders.first().cloned().unwrap_or_default(),
            folders: present,
            missing_folders: missing,
            project: Some(project.clone()),
            id_source: Some(IdSource::State),
            tracking,
            title: main_addon.and_then(|a| a.title.clone()),
            local_version: main_addon.and_then(|a| a.toc_version.clone()),
            managed: Some(managed.clone()),
            warnings,
        });
    }

    // 2. Remaining folders: protected, conflicting, linked, declared, untracked.
    let mut groups: BTreeMap<ProjectRef, (Vec<usize>, IdSource)> = BTreeMap::new();
    for (i, addon) in inv.addons.iter().enumerate() {
        if consumed[i] {
            continue;
        }
        let base = |tracking: Tracking| AddonEntry {
            name: addon.folder.clone(),
            folders: vec![addon.folder.clone()],
            missing_folders: Vec::new(),
            project: None,
            id_source: None,
            tracking,
            title: addon.title.clone(),
            local_version: addon.toc_version.clone(),
            managed: None,
            warnings: addon.warnings.clone(),
        };
        if addon.protected.is_some() {
            entries.push(base(Tracking::Protected));
            continue;
        }
        let (project, source) = match state.link_for(&addon.folder) {
            Some((_, p)) => (Some(p.clone()), IdSource::Link),
            None if addon.has_project_conflict() => {
                entries.push(base(Tracking::Conflict));
                continue;
            }
            None => (addon.declared_projects.first().cloned(), IdSource::Toc),
        };
        match project {
            Some(p) => {
                let g = groups.entry(p).or_insert((Vec::new(), source));
                g.0.push(i);
                if source == IdSource::Link {
                    g.1 = IdSource::Link;
                }
            }
            None => entries.push(base(Tracking::Untracked)),
        }
    }

    for (project, (indexes, source)) in groups {
        let mut folders: Vec<String> = indexes
            .iter()
            .map(|&i| inv.addons[i].folder.clone())
            .collect();
        sort_main_first(&mut folders);
        let main = inv
            .addons
            .iter()
            .find(|a| a.folder == folders[0])
            .expect("main folder comes from the inventory");
        let mut warnings: Vec<String> = indexes
            .iter()
            .flat_map(|&i| inv.addons[i].warnings.iter().cloned())
            .collect();
        if state.projects.contains_key(&project) {
            warnings.push(format!(
                "{project} is already managed through other folders"
            ));
        }
        entries.push(AddonEntry {
            name: folders[0].clone(),
            folders,
            missing_folders: Vec::new(),
            project: Some(project),
            id_source: Some(source),
            tracking: Tracking::Unmanaged,
            title: main.title.clone(),
            local_version: main.toc_version.clone(),
            managed: None,
            warnings,
        });
    }

    entries.sort_by_key(|e| e.name.to_lowercase());
    entries
}
