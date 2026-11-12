//! One module per command. Commands parse nothing themselves: they receive
//! clap structs, call the services and return an `Outcome`.

pub mod auth;
pub mod check;
pub mod config;
pub mod doctor;
pub mod import;
pub mod init;
pub mod link;
pub mod profile;
pub mod remote;
pub mod remove;
pub mod rollback;
pub mod scan;
pub mod update;

use std::path::PathBuf;

use crate::cli::{Cli, Command, GlobalArgs};
use crate::config::{Config, Settings};
use crate::domain::GameProfile;
use crate::error::{AppError, Exit, Result};
use crate::installer::TxContext;
use crate::net::{ApiKey, NetConfig};
use crate::output::Output;
use crate::providers::curseforge::CurseForgeProvider;
use crate::secrets::{KeySource, os_store, resolve_api_key};
use crate::storage::paths::default_work_dir;
use crate::storage::{AppPaths, State};

/// What a command returns: its exit code and its JSON result.
#[derive(Debug)]
pub struct Outcome {
    pub exit: Exit,
    pub data: serde_json::Value,
}

impl Outcome {
    pub fn ok(data: serde_json::Value) -> Self {
        Outcome {
            exit: Exit::Success,
            data,
        }
    }

    pub fn with_exit(exit: Exit, data: serde_json::Value) -> Self {
        Outcome { exit, data }
    }
}

pub struct Ctx<'a> {
    pub global: &'a GlobalArgs,
    pub out: &'a Output,
    pub paths: AppPaths,
}

impl Ctx<'_> {
    pub fn load_config(&self) -> Result<Config> {
        Config::load(&self.paths)
    }

    pub fn profile(&self, config: &Config) -> Result<GameProfile> {
        config.resolve_profile(self.global.profile.as_deref())
    }

    pub fn load_state(&self, profile: &GameProfile) -> Result<State> {
        State::load(&self.paths.state_file(&profile.name), profile.name.as_str())
    }

    pub fn tx_context(&self, profile: &GameProfile, settings: &Settings) -> TxContext {
        TxContext {
            paths: self.paths.clone(),
            profile: profile.name.clone(),
            addons_path: profile.addons_path.clone(),
            work_dir: work_dir(profile),
            backup_enabled: settings.backup_enabled,
            backup_retention: settings.backup_retention,
        }
    }

    /// The API key from the environment or the system secret store.
    pub fn api_key(&self) -> Option<(ApiKey, KeySource)> {
        let resolved = resolve_api_key(os_store().as_ref());
        if let Some(w) = &resolved.warning {
            self.out.warn(w);
        }
        resolved.key
    }

    /// The CurseForge provider, or an error when offline or without an API key.
    pub fn provider(&self, settings: &Settings) -> Result<CurseForgeProvider> {
        if self.global.offline {
            return Err(AppError::Other(
                "this command needs network access, which --offline disables".into(),
            ));
        }
        let (key, _) = self.api_key().ok_or(AppError::MissingApiKey)?;
        self.provider_with_key(settings, key)
    }

    pub fn provider_with_key(
        &self,
        settings: &Settings,
        key: ApiKey,
    ) -> Result<CurseForgeProvider> {
        let cfg = NetConfig {
            timeout: std::time::Duration::from_secs(settings.timeout_seconds),
            max_retries: settings.max_retries,
            ..NetConfig::default()
        };
        CurseForgeProvider::new(key, cfg).map_err(AppError::from)
    }
}

pub fn work_dir(profile: &GameProfile) -> PathBuf {
    profile
        .work_dir
        .clone()
        .unwrap_or_else(|| default_work_dir(&profile.addons_path, &profile.name))
}

pub async fn dispatch(cli: &Cli, out: &Output) -> Result<Outcome> {
    if cli.global.profile.is_some()
        && matches!(
            cli.command,
            Command::Init | Command::Config(_) | Command::Auth(_)
        )
    {
        return Err(AppError::Usage(format!(
            "--profile is not accepted by `{}`",
            cli.command.name()
        )));
    }
    let paths = AppPaths::resolve(cli.global.config.as_deref())?;
    let ctx = Ctx {
        global: &cli.global,
        out,
        paths,
    };
    match &cli.command {
        Command::Init => init::run(&ctx),
        Command::Scan(args) => scan::scan(&ctx, args),
        Command::List(args) => scan::list(&ctx, args),
        Command::Check(args) => check::run(&ctx, args).await,
        Command::Update(args) => update::run(&ctx, args).await,
        Command::Inbox(args) => import::inbox(&ctx, args),
        Command::Import(args) => import::import(&ctx, args),
        Command::Remove(args) => remove::run(&ctx, args),
        Command::Doctor => doctor::run(&ctx).await,
        Command::Link(args) => link::link(&ctx, args).await,
        Command::Unlink(args) => link::unlink(&ctx, args),
        Command::History => rollback::history(&ctx),
        Command::Rollback(args) => rollback::rollback(&ctx, args),
        Command::Config(cmd) => config::run(&ctx, cmd),
        Command::Profile(cmd) => profile::run(&ctx, cmd),
        Command::Auth(cmd) => auth::run(&ctx, cmd).await,
    }
}

/// Game client executables: `Wow.exe`, `WowClassic.exe`, `WowT.exe`, … but not this tool.
/// Linux truncates process names to 15 characters, so `.exe` may be cut off there.
fn is_wow_process(name: &str) -> bool {
    name.starts_with("wow")
        && !name.starts_with("wowaddons")
        && (name.ends_with(".exe") || (name.len() >= 15 && !cfg!(windows)))
}

/// Best-effort check for a running WoW client.
pub fn wow_running() -> bool {
    #[cfg(windows)]
    {
        let Ok(out) = std::process::Command::new("tasklist")
            .args(["/FO", "CSV", "/NH"])
            .output()
        else {
            return false;
        };
        String::from_utf8_lossy(&out.stdout).lines().any(|l| {
            let name = l
                .trim_start_matches('"')
                .split('"')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            is_wow_process(&name)
        })
    }
    #[cfg(not(windows))]
    {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return false;
        };
        entries.flatten().any(|e| {
            std::fs::read_to_string(e.path().join("comm"))
                .map(|c| is_wow_process(&c.trim().to_ascii_lowercase()))
                .unwrap_or(false)
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn wow_process_names() {
        assert!(super::is_wow_process("wow.exe"));
        assert!(super::is_wow_process("wowclassic.exe"));
        assert!(!super::is_wow_process("wowaddons.exe"));
        assert!(!super::is_wow_process("explorer.exe"));
    }
}
