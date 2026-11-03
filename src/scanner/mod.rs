//! Local `AddOns` folder analysis. Read-only.

pub mod group;
pub mod toc;

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::domain::addon::is_curseforge_id;
use crate::domain::{LocalAddon, ProjectRef, ProtectedReason};
use crate::error::{AppError, Result};

pub use group::{AddonEntry, IdSource, build_entries};

/// TOC files larger than this are not read.
const MAX_TOC_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct IgnoredFolder {
    pub name: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Inventory {
    pub addons: Vec<LocalAddon>,
    pub ignored: Vec<IgnoredFolder>,
}

impl Inventory {
    pub fn find(&self, folder: &str) -> Option<&LocalAddon> {
        self.addons
            .iter()
            .find(|a| a.folder.eq_ignore_ascii_case(folder))
    }
}

/// Scan the direct children of `addons_path`.
/// `suffixes` are the active client's TOC suffixes, most specific first.
pub fn scan(addons_path: &Path, suffixes: &[String]) -> Result<Inventory> {
    let entries = std::fs::read_dir(addons_path)
        .map_err(|e| AppError::io("cannot read the AddOns folder", addons_path, e))?;
    let mut inv = Inventory::default();
    for entry in entries {
        let entry =
            entry.map_err(|e| AppError::io("cannot read the AddOns folder", addons_path, e))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let ty = match entry.file_type() {
            Ok(t) => t,
            Err(e) => {
                inv.ignored.push(IgnoredFolder {
                    name,
                    reason: format!("cannot read: {e}"),
                });
                continue;
            }
        };
        if ty.is_symlink() || is_reparse_point(&path) {
            inv.addons
                .push(protected_addon(name, path, ProtectedReason::Symlink));
            continue;
        }
        if !ty.is_dir() {
            continue;
        }
        if name.to_ascii_lowercase().starts_with("blizzard_") {
            inv.addons
                .push(protected_addon(name, path, ProtectedReason::Blizzard));
            continue;
        }
        match read_addon(&name, &path, suffixes) {
            Ok(Some(addon)) => inv.addons.push(addon),
            Ok(None) => inv.ignored.push(IgnoredFolder {
                name,
                reason: "no matching .toc file".into(),
            }),
            Err(e) => inv.ignored.push(IgnoredFolder {
                name,
                reason: format!("cannot read: {e}"),
            }),
        }
    }
    inv.addons.sort_by_key(|a| a.folder.to_lowercase());
    inv.ignored.sort_by_key(|a| a.name.to_lowercase());
    Ok(inv)
}

fn protected_addon(folder: String, path: PathBuf, reason: ProtectedReason) -> LocalAddon {
    LocalAddon {
        folder,
        path,
        title: None,
        toc_version: None,
        interface_versions: Vec::new(),
        selected_toc: None,
        toc_files: Vec::new(),
        declared_projects: Vec::new(),
        protected: Some(reason),
        warnings: Vec::new(),
    }
}

/// Windows junctions are reported as symlinks by std on recent toolchains;
/// this extra check covers other reparse points.
#[cfg(windows)]
fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn is_reparse_point(_path: &Path) -> bool {
    false
}

/// TOC files of a folder: `<Folder>.toc` (suffix `None`) or `<Folder>_<Suffix>.toc`.
pub fn toc_files(folder: &str, path: &Path) -> std::io::Result<Vec<(String, Option<String>)>> {
    let lower = folder.to_lowercase();
    let mut out = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let file = entry.file_name().to_string_lossy().to_string();
        let Some(stem) = strip_toc_ext(&file) else {
            continue;
        };
        let stem_lower = stem.to_lowercase();
        if stem_lower == lower {
            out.push((file, None));
        } else if let Some(suffix) = stem_lower.strip_prefix(&format!("{lower}_"))
            && !suffix.is_empty()
        {
            out.push((file.clone(), Some(stem[lower.len() + 1..].to_string())));
        }
    }
    out.sort();
    Ok(out)
}

fn strip_toc_ext(file: &str) -> Option<&str> {
    let n = file.len();
    (n > 4 && file[n - 4..].eq_ignore_ascii_case(".toc")).then(|| &file[..n - 4])
}

/// Pick the TOC the client would load: first matching suffix, then the base file.
pub fn select_toc<'a>(
    tocs: &'a [(String, Option<String>)],
    suffixes: &[String],
) -> Option<&'a (String, Option<String>)> {
    for suffix in suffixes {
        if let Some(t) = tocs
            .iter()
            .find(|(_, s)| s.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(suffix)))
        {
            return Some(t);
        }
    }
    tocs.iter().find(|(_, s)| s.is_none())
}

fn read_addon(
    folder: &str,
    path: &Path,
    suffixes: &[String],
) -> std::io::Result<Option<LocalAddon>> {
    let tocs = toc_files(folder, path)?;
    if tocs.is_empty() {
        return Ok(None);
    }
    let mut warnings = Vec::new();
    let selected = match select_toc(&tocs, suffixes) {
        Some(t) => t.clone(),
        None => {
            let first = tocs[0].clone();
            warnings.push(format!(
                "no TOC file for this client; the game may not load this addon (read {})",
                first.0
            ));
            first
        }
    };

    let mut declared: Vec<ProjectRef> = Vec::new();
    let mut selected_data = None;
    for (file, _) in &tocs {
        let toc_path = path.join(file);
        let meta = std::fs::metadata(&toc_path)?;
        if meta.len() > MAX_TOC_BYTES {
            warnings.push(format!("{file} is too large and was not read"));
            continue;
        }
        let data = toc::parse_toc(&std::fs::read(&toc_path)?);
        if let Some(id) = data.curse_project_id() {
            if is_curseforge_id(id) {
                let p = ProjectRef::curseforge(id);
                if !declared.contains(&p) {
                    declared.push(p);
                }
            } else {
                warnings.push(format!("{file}: invalid X-Curse-Project-ID \"{id}\""));
            }
        }
        if *file == selected.0 {
            selected_data = Some(data);
        }
    }
    if declared.len() > 1 {
        warnings.push(format!(
            "TOC files disagree on the project ID ({})",
            declared
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let data = selected_data.unwrap_or_default();
    if data.has_version_placeholder() {
        warnings.push("version is an unreplaced @project-version@ placeholder".into());
    }
    let (interfaces, bad_interface) = data.interfaces();
    if bad_interface {
        warnings.push("invalid value in ## Interface".into());
    }

    Ok(Some(LocalAddon {
        folder: folder.to_string(),
        path: path.to_path_buf(),
        title: data.title(),
        toc_version: data.version().map(str::to_string),
        interface_versions: interfaces,
        selected_toc: Some(selected.0),
        toc_files: tocs.into_iter().map(|(f, _)| f).collect(),
        declared_projects: declared,
        protected: None,
        warnings,
    }))
}

/// Is `path` a folder containing a TOC file that matches its name?
pub fn is_addon_folder(name: &str, path: &Path) -> bool {
    toc_files(name, path)
        .map(|t| !t.is_empty())
        .unwrap_or(false)
}
