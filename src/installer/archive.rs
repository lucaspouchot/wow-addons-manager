//! Archive integrity and safety checks, then extraction into staging.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use crate::domain::RemoteFile;
use crate::providers::DownloadedArchive;
use crate::scanner::is_addon_folder;

#[derive(Clone, Copy, Debug)]
pub struct ArchiveLimits {
    pub max_entries: u64,
    pub max_extracted_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ArchiveError(pub String);

fn err(msg: impl Into<String>) -> ArchiveError {
    ArchiveError(msg.into())
}

/// Root folders extracted under `root`.
#[derive(Clone, Debug)]
pub struct ExtractedArchive {
    pub root: PathBuf,
    pub folders: Vec<String>,
}

/// Check the downloaded size and SHA-1 against the provider's metadata.
/// Returns a warning when no hash was available.
pub fn verify_download(
    archive: &DownloadedArchive,
    file: &RemoteFile,
) -> Result<Option<String>, ArchiveError> {
    if let Some(expected) = file.size
        && archive.size != expected
    {
        return Err(err(format!(
            "size mismatch (expected {expected} bytes, got {})",
            archive.size
        )));
    }
    match &file.sha1 {
        Some(expected) if !expected.eq_ignore_ascii_case(&archive.sha1) => {
            Err(err("invalid SHA-1 checksum"))
        }
        Some(_) => Ok(None),
        None => Ok(Some(
            "no SHA-1 hash was provided for this file; integrity not verified".into(),
        )),
    }
}

/// Size and lowercase hex SHA-1 of a local file, read in one pass.
pub fn hash_file(path: &Path) -> std::io::Result<(u64, String)> {
    use sha1::{Digest, Sha1};
    let mut file = BufReader::new(File::open(path)?);
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((size, hex::encode(hasher.finalize())))
}

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validate one entry name and split it into components.
pub fn check_entry_name(name: &str) -> Result<Vec<String>, ArchiveError> {
    let bad = |why: &str| err(format!("unsafe path in archive \"{name}\": {why}"));
    if name.is_empty() {
        return Err(bad("empty name"));
    }
    if name.contains('\\') {
        return Err(bad("contains a backslash"));
    }
    if name.starts_with('/') {
        return Err(bad("absolute path"));
    }
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    let mut out = Vec::new();
    for comp in trimmed.split('/') {
        if comp.is_empty() {
            return Err(bad("empty path component"));
        }
        if comp == ".." {
            return Err(bad("parent directory component"));
        }
        if comp == "." {
            return Err(bad("current directory component"));
        }
        if comp
            .chars()
            .any(|c| c.is_control() || "<>:\"|?*".contains(c))
        {
            return Err(bad("character not allowed on Windows"));
        }
        if comp.ends_with('.') || comp.ends_with(' ') {
            return Err(bad("name ends with a dot or a space"));
        }
        let stem = comp
            .split('.')
            .next()
            .unwrap_or(comp)
            .trim_end()
            .to_ascii_uppercase();
        if RESERVED.contains(&stem.as_str()) {
            return Err(bad("reserved name on Windows"));
        }
        out.push(comp.to_string());
    }
    Ok(out)
}

struct Entry {
    index: usize,
    components: Vec<String>,
    is_dir: bool,
}

/// Validate every entry, then extract into `dest` (which must not exist yet or be empty).
pub fn extract(
    zip_path: &Path,
    dest: &Path,
    limits: ArchiveLimits,
) -> Result<ExtractedArchive, ArchiveError> {
    let file = File::open(zip_path).map_err(|e| err(format!("cannot open archive: {e}")))?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| err(format!("invalid ZIP archive: {e}")))?;
    if zip.len() as u64 > limits.max_entries {
        return Err(err(format!(
            "archive has {} entries (limit {})",
            zip.len(),
            limits.max_entries
        )));
    }

    // Pass 1: names, types, collisions. Nothing is written.
    let mut entries = Vec::with_capacity(zip.len());
    let mut seen: HashMap<String, (String, bool)> = HashMap::new();
    let mut roots: Vec<String> = Vec::new();
    for i in 0..zip.len() {
        let f = zip
            .by_index(i)
            .map_err(|e| err(format!("invalid ZIP entry: {e}")))?;
        if f.encrypted() {
            return Err(err("archive contains encrypted entries"));
        }
        if f.is_symlink() {
            return Err(err(format!(
                "archive contains a symbolic link ({})",
                f.name().unwrap_or_default()
            )));
        }
        let name = f
            .name()
            .map_err(|_| err("archive contains a name that is not valid UTF-8"))?
            .to_string();
        let components = check_entry_name(&name)?;
        let is_dir = f.is_dir();
        if !is_dir && components.len() == 1 {
            return Err(err(format!(
                "archive has a file at its root (\"{name}\"); expected only folders"
            )));
        }
        for k in 1..=components.len() {
            let path = components[..k].join("/");
            let as_dir = k < components.len() || is_dir;
            match seen.get(&path.to_lowercase()) {
                Some((exact, _)) if *exact != path => {
                    return Err(err(format!(
                        "archive entries clash when case is ignored: \"{exact}\" and \"{path}\""
                    )));
                }
                Some((_, was_dir)) if !(*was_dir && as_dir) => {
                    return Err(err(format!("duplicate entry in archive: \"{path}\"")));
                }
                Some(_) => {}
                None => {
                    seen.insert(path.to_lowercase(), (path.clone(), as_dir));
                }
            }
        }
        if !roots.contains(&components[0]) {
            roots.push(components[0].clone());
        }
        entries.push(Entry {
            index: i,
            components,
            is_dir,
        });
    }
    if roots.is_empty() {
        return Err(err("archive is empty"));
    }

    // Pass 2: extraction with a running size limit.
    std::fs::create_dir_all(dest).map_err(|e| err(format!("cannot create staging folder: {e}")))?;
    let mut total: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    for entry in &entries {
        let mut out_path = dest.to_path_buf();
        for c in &entry.components {
            out_path.push(c);
        }
        if !out_path.starts_with(dest) {
            return Err(err("entry escapes the staging folder"));
        }
        if entry.is_dir {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| err(format!("cannot create folder: {e}")))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| err(format!("cannot create folder: {e}")))?;
        }
        let mut f = zip
            .by_index(entry.index)
            .map_err(|e| err(format!("invalid ZIP entry: {e}")))?;
        let mut out = File::create_new(&out_path)
            .map_err(|e| err(format!("cannot create {}: {e}", out_path.display())))?;
        loop {
            let n = f
                .read(&mut buf)
                .map_err(|e| err(format!("corrupt archive: {e}")))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > limits.max_extracted_bytes {
                return Err(err(format!(
                    "archive expands beyond the limit ({} MB)",
                    limits.max_extracted_bytes / (1024 * 1024)
                )));
            }
            out.write_all(&buf[..n])
                .map_err(|e| err(format!("cannot write {}: {e}", out_path.display())))?;
        }
    }

    roots.sort_by_key(|r| r.to_lowercase());
    for root in &roots {
        if !is_addon_folder(root, &dest.join(root)) {
            return Err(err(format!(
                "archive folder \"{root}\" is not a valid addon (no matching .toc file)"
            )));
        }
    }
    Ok(ExtractedArchive {
        root: dest.to_path_buf(),
        folders: roots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(check_entry_name("A/b.lua").unwrap(), vec!["A", "b.lua"]);
        assert_eq!(check_entry_name("A/").unwrap(), vec!["A"]);
        assert!(check_entry_name("A/..foo/x").is_ok());
        for bad in [
            "../x",
            "A/../../x",
            "/etc/passwd",
            "C:/x",
            "A\\b",
            "A//b",
            "A/./b",
            "A/CON",
            "A/nul.txt",
            "A/b.",
            "A/b ",
            "A/b?c",
            "",
        ] {
            assert!(check_entry_name(bad).is_err(), "{bad}");
        }
    }
}
