//! Configuration file: reading, validation and edits.
//!
//! Reads go through typed `serde` structs (unknown keys are errors).
//! Writes go through `toml_edit` so comments and formatting are kept.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml_edit::{DocumentMut, Item, Table};

use crate::domain::{GameProfile, ProfileName, ReleaseChannel, WowClient};
use crate::error::{AppError, Result};
use crate::storage::fsutil::atomic_write;
use crate::storage::paths::path_key;
use crate::storage::{AppPaths, FileLock};

pub const CONFIG_VERSION: u32 = 1;

fn default_config_version() -> u32 {
    CONFIG_VERSION
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default = "default_config_version")]
    pub config_version: u32,
    pub default_profile: Option<String>,
    #[serde(default)]
    pub defaults: DefaultsSection,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileSection>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultsSection {
    pub channel: Option<ReleaseChannel>,
    pub auto_confirm: Option<bool>,
    pub backup_enabled: Option<bool>,
    pub backup_retention: Option<i64>,
    pub timeout_seconds: Option<i64>,
    pub max_retries: Option<i64>,
    pub max_parallel_downloads: Option<i64>,
    pub max_archive_size_mb: Option<i64>,
    pub max_extracted_size_mb: Option<i64>,
    pub max_archive_entries: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSection {
    pub client: String,
    pub addons_path: PathBuf,
    pub game_version: Option<String>,
    pub game_version_type_id: Option<i64>,
    pub channel: Option<ReleaseChannel>,
    pub toc_suffixes: Option<Vec<String>>,
    pub work_dir: Option<PathBuf>,
}

/// Effective global settings (`[defaults]` merged with built-in defaults).
#[derive(Clone, Debug, serde::Serialize)]
pub struct Settings {
    pub channel: ReleaseChannel,
    pub auto_confirm: bool,
    pub backup_enabled: bool,
    pub backup_retention: u32,
    pub timeout_seconds: u64,
    pub max_retries: u32,
    pub max_parallel_downloads: usize,
    pub max_archive_size_mb: u64,
    pub max_extracted_size_mb: u64,
    pub max_archive_entries: u64,
}

// ---------------------------------------------------------------------------
// Key registry
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub enum KeyType {
    Channel,
    Bool,
    Int { min: i64, max: i64 },
    Text,
    Client,
    Path,
    List,
    Profile,
}

#[derive(Clone, Copy, Debug)]
pub struct KeySpec {
    pub key: &'static str,
    pub ty: KeyType,
    /// Built-in default shown by `config list`, if any.
    pub default: Option<&'static str>,
}

pub const GLOBAL_KEYS: &[KeySpec] = &[
    KeySpec {
        key: "default_profile",
        ty: KeyType::Profile,
        default: None,
    },
    KeySpec {
        key: "defaults.channel",
        ty: KeyType::Channel,
        default: Some("release"),
    },
    KeySpec {
        key: "defaults.auto_confirm",
        ty: KeyType::Bool,
        default: Some("false"),
    },
    KeySpec {
        key: "defaults.backup_enabled",
        ty: KeyType::Bool,
        default: Some("true"),
    },
    KeySpec {
        key: "defaults.backup_retention",
        ty: KeyType::Int { min: 1, max: 1000 },
        default: Some("5"),
    },
    KeySpec {
        key: "defaults.timeout_seconds",
        ty: KeyType::Int { min: 1, max: 600 },
        default: Some("30"),
    },
    KeySpec {
        key: "defaults.max_retries",
        ty: KeyType::Int { min: 0, max: 10 },
        default: Some("3"),
    },
    KeySpec {
        key: "defaults.max_parallel_downloads",
        ty: KeyType::Int { min: 1, max: 8 },
        default: Some("3"),
    },
    KeySpec {
        key: "defaults.max_archive_size_mb",
        ty: KeyType::Int {
            min: 1,
            max: 10_000,
        },
        default: Some("200"),
    },
    KeySpec {
        key: "defaults.max_extracted_size_mb",
        ty: KeyType::Int {
            min: 1,
            max: 100_000,
        },
        default: Some("1024"),
    },
    KeySpec {
        key: "defaults.max_archive_entries",
        ty: KeyType::Int {
            min: 1,
            max: 1_000_000,
        },
        default: Some("20000"),
    },
];

pub const PROFILE_KEYS: &[KeySpec] = &[
    KeySpec {
        key: "client",
        ty: KeyType::Client,
        default: None,
    },
    KeySpec {
        key: "addons_path",
        ty: KeyType::Path,
        default: None,
    },
    KeySpec {
        key: "game_version",
        ty: KeyType::Text,
        default: None,
    },
    KeySpec {
        key: "game_version_type_id",
        ty: KeyType::Int {
            min: 1,
            max: u32::MAX as i64,
        },
        default: None,
    },
    KeySpec {
        key: "channel",
        ty: KeyType::Channel,
        default: None,
    },
    KeySpec {
        key: "toc_suffixes",
        ty: KeyType::List,
        default: None,
    },
    KeySpec {
        key: "work_dir",
        ty: KeyType::Path,
        default: None,
    },
];

fn find_key(keys: &[KeySpec], key: &str) -> Result<KeySpec> {
    keys.iter().copied().find(|k| k.key == key).ok_or_else(|| {
        let names: Vec<_> = keys.iter().map(|k| k.key).collect();
        AppError::Usage(format!(
            "unknown key \"{key}\" (valid keys: {})",
            names.join(", ")
        ))
    })
}

pub fn global_key(key: &str) -> Result<KeySpec> {
    find_key(GLOBAL_KEYS, key)
}

pub fn profile_key(key: &str) -> Result<KeySpec> {
    find_key(PROFILE_KEYS, key)
}

/// Parse a CLI string into a TOML value of the right type.
fn parse_value(spec: &KeySpec, raw: &str) -> Result<toml_edit::Value> {
    let bad = |msg: String| AppError::Usage(format!("invalid value for {}: {msg}", spec.key));
    Ok(match spec.ty {
        KeyType::Channel => {
            let c: ReleaseChannel = raw.parse().map_err(bad)?;
            c.as_str().into()
        }
        KeyType::Bool => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => true.into(),
            "false" | "no" | "off" | "0" => false.into(),
            other => return Err(bad(format!("\"{other}\" is not a boolean (true/false)"))),
        },
        KeyType::Int { min, max } => {
            let n: i64 = raw
                .trim()
                .parse()
                .map_err(|_| bad(format!("\"{raw}\" is not an integer")))?;
            if n < min || n > max {
                return Err(bad(format!("{n} is out of range {min}..={max}")));
            }
            n.into()
        }
        KeyType::Text | KeyType::Profile => {
            let t = raw.trim();
            if t.is_empty() {
                return Err(bad("value cannot be empty".into()));
            }
            t.into()
        }
        KeyType::Client => {
            let c: WowClient = raw.parse().map_err(bad)?;
            c.id().into()
        }
        KeyType::Path => {
            if raw.trim().is_empty() {
                return Err(bad("path cannot be empty".into()));
            }
            raw.trim().into()
        }
        KeyType::List => {
            let mut arr = toml_edit::Array::new();
            for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                arr.push(part);
            }
            arr.into()
        }
    })
}

// ---------------------------------------------------------------------------
// Loading and validation
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Config {
    pub file: ConfigFile,
    pub path: PathBuf,
    pub exists: bool,
}

impl Config {
    pub fn load(paths: &AppPaths) -> Result<Config> {
        let path = paths.config_file.clone();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Config {
                    file: ConfigFile {
                        config_version: CONFIG_VERSION,
                        ..ConfigFile::default()
                    },
                    path,
                    exists: false,
                });
            }
            Err(e) => return Err(AppError::io("cannot read config", &path, e)),
        };
        let file = parse_config_text(&text, &path)?;
        Ok(Config {
            file,
            path,
            exists: true,
        })
    }

    pub fn settings(&self) -> Settings {
        let d = &self.file.defaults;
        Settings {
            channel: d.channel.unwrap_or(ReleaseChannel::Release),
            auto_confirm: d.auto_confirm.unwrap_or(false),
            backup_enabled: d.backup_enabled.unwrap_or(true),
            backup_retention: d.backup_retention.unwrap_or(5) as u32,
            timeout_seconds: d.timeout_seconds.unwrap_or(30) as u64,
            max_retries: d.max_retries.unwrap_or(3) as u32,
            max_parallel_downloads: d.max_parallel_downloads.unwrap_or(3) as usize,
            max_archive_size_mb: d.max_archive_size_mb.unwrap_or(200) as u64,
            max_extracted_size_mb: d.max_extracted_size_mb.unwrap_or(1024) as u64,
            max_archive_entries: d.max_archive_entries.unwrap_or(20_000) as u64,
        }
    }

    pub fn profile_names(&self) -> Vec<String> {
        self.file.profiles.keys().cloned().collect()
    }

    pub fn profile(&self, name: &str) -> Result<GameProfile> {
        let section = self
            .file
            .profiles
            .get(name)
            .ok_or_else(|| AppError::ProfileNotFound(name.to_string()))?;
        to_profile(name, section)
    }

    /// `--profile` if given, otherwise `default_profile`.
    pub fn resolve_profile(&self, cli: Option<&str>) -> Result<GameProfile> {
        match cli.or(self.file.default_profile.as_deref()) {
            Some(name) => self.profile(name),
            None => Err(AppError::NoProfile),
        }
    }
}

fn parse_config_text(text: &str, path: &Path) -> Result<ConfigFile> {
    let file: ConfigFile =
        toml::from_str(text).map_err(|e| AppError::Config(format!("{}:\n{e}", path.display())))?;
    validate(&file).map_err(|e| AppError::Config(format!("{}: {e}", path.display())))?;
    Ok(file)
}

fn to_profile(name: &str, s: &ProfileSection) -> Result<GameProfile> {
    let name: ProfileName = name.parse().map_err(AppError::Config)?;
    let client: WowClient = s.client.parse().map_err(AppError::Config)?;
    Ok(GameProfile {
        name,
        client,
        addons_path: s.addons_path.clone(),
        work_dir: s.work_dir.clone(),
        game_version: s.game_version.clone(),
        game_version_type_id: s.game_version_type_id.map(|v| v as u32),
        channel: s.channel,
        toc_suffixes: s.toc_suffixes.clone().unwrap_or_default(),
    })
}

/// Semantic checks that `serde` cannot express.
pub fn validate(file: &ConfigFile) -> std::result::Result<(), String> {
    if file.config_version == 0 {
        return Err("config_version must be at least 1".into());
    }
    if file.config_version > CONFIG_VERSION {
        return Err(format!(
            "config_version {} was written by a newer version of wowaddons (supported: {CONFIG_VERSION})",
            file.config_version
        ));
    }
    if let Some(dp) = &file.default_profile
        && !file.profiles.contains_key(dp)
    {
        return Err(format!("default_profile \"{dp}\" does not exist"));
    }
    let d = &file.defaults;
    let ints = [
        ("defaults.backup_retention", d.backup_retention),
        ("defaults.timeout_seconds", d.timeout_seconds),
        ("defaults.max_retries", d.max_retries),
        ("defaults.max_parallel_downloads", d.max_parallel_downloads),
        ("defaults.max_archive_size_mb", d.max_archive_size_mb),
        ("defaults.max_extracted_size_mb", d.max_extracted_size_mb),
        ("defaults.max_archive_entries", d.max_archive_entries),
    ];
    for (key, v) in ints {
        if let (Some(v), Ok(spec)) = (v, global_key(key))
            && let KeyType::Int { min, max } = spec.ty
            && (v < min || v > max)
        {
            return Err(format!("{key} = {v} is out of range {min}..={max}"));
        }
    }

    let mut seen_paths: HashMap<String, &str> = HashMap::new();
    for (name, p) in &file.profiles {
        name.parse::<ProfileName>()?;
        let client: WowClient = p
            .client
            .parse()
            .map_err(|e| format!("profile \"{name}\": {e}"))?;
        if p.addons_path.as_os_str().is_empty() {
            return Err(format!("profile \"{name}\": addons_path is empty"));
        }
        if let Some(id) = p.game_version_type_id
            && (id < 1 || id > u32::MAX as i64)
        {
            return Err(format!(
                "profile \"{name}\": game_version_type_id is out of range"
            ));
        }
        if p.game_version
            .as_deref()
            .is_some_and(|v| v.trim().is_empty())
        {
            return Err(format!("profile \"{name}\": game_version is empty"));
        }
        if let Some(suffixes) = &p.toc_suffixes
            && let Some(bad) = suffixes
                .iter()
                .find(|s| s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric()))
        {
            return Err(format!("profile \"{name}\": invalid TOC suffix \"{bad}\""));
        }
        // A custom client without toc_suffixes only reads the base `<Folder>.toc`.
        if client == WowClient::Custom && p.game_version_type_id.is_none() {
            return Err(format!(
                "profile \"{name}\": a custom client needs game_version_type_id"
            ));
        }
        let key = path_key(&p.addons_path);
        if let Some(other) = seen_paths.insert(key, name) {
            return Err(format!(
                "profiles \"{other}\" and \"{name}\" use the same addons_path"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Editing
// ---------------------------------------------------------------------------

const TEMPLATE: &str = "\
# wowaddons configuration.
# Run `wowaddons config list` to see every key and its effective value.
config_version = 1

# Global defaults, for example:
#   channel = \"release\"
#   backup_retention = 5
[defaults]
";

/// Edits the config file while keeping comments and formatting.
pub struct ConfigEditor {
    doc: DocumentMut,
    path: PathBuf,
    lock_path: PathBuf,
}

impl ConfigEditor {
    pub fn open(paths: &AppPaths) -> Result<ConfigEditor> {
        let path = paths.config_file.clone();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => TEMPLATE.to_string(),
            Err(e) => return Err(AppError::io("cannot read config", &path, e)),
        };
        // Make sure the current file is valid before editing it.
        parse_config_text(&text, &path)?;
        let doc: DocumentMut = text
            .parse()
            .map_err(|e| AppError::Config(format!("{}:\n{e}", path.display())))?;
        Ok(ConfigEditor {
            doc,
            path,
            lock_path: paths.config_lock_file(),
        })
    }

    pub fn template() -> &'static str {
        TEMPLATE
    }

    fn split_global(key: &str) -> (Option<&str>, &str) {
        match key.split_once('.') {
            Some((table, k)) => (Some(table), k),
            None => (None, key),
        }
    }

    pub fn set_global(&mut self, key: &str, raw: &str) -> Result<()> {
        let spec = global_key(key)?;
        let v = parse_value(&spec, raw)?;
        match Self::split_global(key) {
            (Some(table), k) => {
                let t = ensure_table(self.doc.as_table_mut(), table, false);
                t[k] = Item::Value(v);
            }
            (None, k) => {
                self.doc[k] = Item::Value(v);
            }
        }
        Ok(())
    }

    pub fn unset_global(&mut self, key: &str) -> Result<()> {
        global_key(key)?;
        match Self::split_global(key) {
            (Some(table), k) => {
                if let Some(t) = self.doc.get_mut(table).and_then(Item::as_table_like_mut) {
                    t.remove(k);
                }
            }
            (None, k) => {
                self.doc.remove(k);
            }
        }
        Ok(())
    }

    fn profile_table(&mut self, name: &str) -> Result<&mut Table> {
        self.doc
            .get_mut("profiles")
            .and_then(Item::as_table_mut)
            .and_then(|t| t.get_mut(name))
            .and_then(Item::as_table_mut)
            .ok_or_else(|| AppError::ProfileNotFound(name.to_string()))
    }

    pub fn has_profile(&self, name: &str) -> bool {
        self.doc
            .get("profiles")
            .and_then(Item::as_table)
            .is_some_and(|t| t.contains_key(name))
    }

    pub fn add_profile(&mut self, name: &str, entries: &[(&str, String)]) -> Result<()> {
        if self.has_profile(name) {
            return Err(AppError::Usage(format!(
                "profile \"{name}\" already exists"
            )));
        }
        let mut table = Table::new();
        for (key, raw) in entries {
            let spec = profile_key(key)?;
            table[*key] = Item::Value(parse_value(&spec, raw)?);
        }
        let profiles = ensure_table(self.doc.as_table_mut(), "profiles", true);
        profiles.insert(name, Item::Table(table));
        Ok(())
    }

    pub fn remove_profile(&mut self, name: &str) -> Result<()> {
        let removed = self
            .doc
            .get_mut("profiles")
            .and_then(Item::as_table_mut)
            .and_then(|t| t.remove(name));
        if removed.is_none() {
            return Err(AppError::ProfileNotFound(name.to_string()));
        }
        if self.doc.get("default_profile").and_then(Item::as_str) == Some(name) {
            self.doc.remove("default_profile");
        }
        Ok(())
    }

    pub fn set_profile(&mut self, name: &str, key: &str, raw: &str) -> Result<()> {
        let spec = profile_key(key)?;
        let v = parse_value(&spec, raw)?;
        let t = self.profile_table(name)?;
        t[key] = Item::Value(v);
        Ok(())
    }

    pub fn unset_profile(&mut self, name: &str, key: &str) -> Result<()> {
        profile_key(key)?;
        if matches!(key, "client" | "addons_path") {
            return Err(AppError::Usage(format!(
                "\"{key}\" is required and cannot be unset"
            )));
        }
        let t = self.profile_table(name)?;
        t.remove(key);
        Ok(())
    }

    /// Validate the edited document, then write it atomically under the config lock.
    pub fn save(self) -> Result<Config> {
        let text = self.doc.to_string();
        let file: ConfigFile = toml::from_str(&text).map_err(|e| {
            AppError::Usage(format!("the change would make the config invalid: {e}"))
        })?;
        validate(&file).map_err(AppError::Usage)?;
        let _lock = FileLock::acquire(&self.lock_path, "config")?;
        atomic_write(&self.path, text.as_bytes())?;
        Ok(Config {
            file,
            path: self.path,
            exists: true,
        })
    }
}

fn ensure_table<'a>(root: &'a mut Table, name: &str, implicit: bool) -> &'a mut Table {
    if !root.get(name).is_some_and(Item::is_table) {
        let mut t = Table::new();
        t.set_implicit(implicit);
        root.insert(name, Item::Table(t));
    }
    root.get_mut(name)
        .and_then(Item::as_table_mut)
        .expect("table was just inserted")
}

/// Read a global key's effective value and where it comes from.
pub fn get_global(config: &Config, key: &str) -> Result<(Option<String>, &'static str)> {
    let spec = global_key(key)?;
    let d = &config.file.defaults;
    let explicit: Option<String> = match key {
        "default_profile" => config.file.default_profile.clone(),
        "defaults.channel" => d.channel.map(|c| c.to_string()),
        "defaults.auto_confirm" => d.auto_confirm.map(|b| b.to_string()),
        "defaults.backup_enabled" => d.backup_enabled.map(|b| b.to_string()),
        "defaults.backup_retention" => d.backup_retention.map(|v| v.to_string()),
        "defaults.timeout_seconds" => d.timeout_seconds.map(|v| v.to_string()),
        "defaults.max_retries" => d.max_retries.map(|v| v.to_string()),
        "defaults.max_parallel_downloads" => d.max_parallel_downloads.map(|v| v.to_string()),
        "defaults.max_archive_size_mb" => d.max_archive_size_mb.map(|v| v.to_string()),
        "defaults.max_extracted_size_mb" => d.max_extracted_size_mb.map(|v| v.to_string()),
        "defaults.max_archive_entries" => d.max_archive_entries.map(|v| v.to_string()),
        _ => None,
    };
    Ok(match explicit {
        Some(v) => (Some(v), "config"),
        None => (spec.default.map(str::to_string), "default"),
    })
}

/// Read a profile key's value, if set.
pub fn get_profile_value(section: &ProfileSection, key: &str) -> Result<Option<String>> {
    profile_key(key)?;
    Ok(match key {
        "client" => Some(section.client.clone()),
        "addons_path" => Some(section.addons_path.display().to_string()),
        "game_version" => section.game_version.clone(),
        "game_version_type_id" => section.game_version_type_id.map(|v| v.to_string()),
        "channel" => section.channel.map(|c| c.to_string()),
        "toc_suffixes" => section.toc_suffixes.as_ref().map(|s| s.join(",")),
        "work_dir" => section.work_dir.as_ref().map(|p| p.display().to_string()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<ConfigFile> {
        parse_config_text(text, Path::new("config.toml"))
    }

    #[test]
    fn full_example_parses() {
        let text = r#"
config_version = 1
default_profile = "forever"

[defaults]
channel = "release"
auto_confirm = false
backup_enabled = true
backup_retention = 5
timeout_seconds = 30
max_parallel_downloads = 3

[profiles.forever]
client = "forever"
addons_path = "C:/Games/World of Warcraft/_forever_/Interface/AddOns"
game_version_type_id = 88568
channel = "release"

[profiles.retail]
client = "retail"
addons_path = "C:/Games/World of Warcraft/_retail_/Interface/AddOns"
"#;
        let f = parse(text).unwrap();
        assert_eq!(f.profiles.len(), 2);
        assert_eq!(f.profiles["forever"].game_version_type_id, Some(88568));
    }

    #[test]
    fn unknown_key_is_an_error() {
        let err = parse("config_version = 1\n[defaults]\nchanel = \"beta\"\n").unwrap_err();
        assert!(err.to_string().contains("chanel"), "{err}");
    }

    #[test]
    fn newer_version_is_an_error() {
        let err = parse("config_version = 2\n").unwrap_err();
        assert!(err.to_string().contains("newer version"), "{err}");
    }

    #[test]
    fn invalid_toml_is_an_error() {
        assert!(parse("config_version = \n").is_err());
    }

    #[test]
    fn missing_default_profile() {
        assert!(parse("default_profile = \"x\"\n").is_err());
    }

    #[test]
    fn duplicate_paths() {
        let text = "[profiles.a]\nclient = \"retail\"\naddons_path = \"/x/AddOns\"\n\
                    [profiles.b]\nclient = \"forever\"\naddons_path = \"/x/AddOns/\"\n";
        assert!(
            parse(text)
                .unwrap_err()
                .to_string()
                .contains("same addons_path")
        );
    }

    #[test]
    fn custom_client_needs_extra_keys() {
        let text = "[profiles.a]\nclient = \"custom\"\naddons_path = \"/x\"\n";
        assert!(parse(text).is_err());
        let text = "[profiles.a]\nclient = \"custom\"\naddons_path = \"/x\"\n\
                    game_version_type_id = 5\ntoc_suffixes = [\"Foo\"]\n";
        assert!(parse(text).is_ok());
        let text = "[profiles.a]\nclient = \"custom\"\naddons_path = \"/x\"\ntoc_suffixes = [\"F-o\"]\n\
                    game_version_type_id = 5\n";
        assert!(parse(text).is_err());
    }

    #[test]
    fn value_parsing() {
        let spec = global_key("defaults.backup_retention").unwrap();
        assert!(parse_value(&spec, "0").is_err());
        assert!(parse_value(&spec, "abc").is_err());
        assert!(parse_value(&spec, "10").is_ok());
        let spec = global_key("defaults.channel").unwrap();
        assert!(parse_value(&spec, "nightly").is_err());
        assert!(global_key("defaults.nope").is_err());
    }
}
