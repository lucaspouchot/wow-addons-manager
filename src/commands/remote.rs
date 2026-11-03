//! Shared remote logic for `check` and `update`: criteria resolution and evaluation.

use std::collections::HashMap;

use futures_util::StreamExt;
use futures_util::stream;
use serde::Serialize;

use crate::cli::FilterArgs;
use crate::config::Settings;
use crate::domain::{
    CheckStatus, GameProfile, ProjectRef, ReleaseChannel, RemoteFile, Tracking, WowClient,
};
use crate::error::{AppError, Result};
use crate::output::Output;
use crate::providers::{AddonProvider, FileQuery, ProviderError, RemoteProject};
use crate::resolver::{self, Criteria, Decision};
use crate::scanner::AddonEntry;

const LOOKUP_CONCURRENCY: usize = 4;

#[derive(Clone, Debug, Serialize)]
pub struct Resolved {
    pub client: WowClient,
    pub version_type_id: u32,
    /// The profile's own version type, when it can be resolved.
    pub profile_version_type_id: Option<u32>,
    pub game_version: Option<String>,
    pub channel: ReleaseChannel,
}

impl Resolved {
    pub fn criteria(&self, allow_client_mismatch: bool) -> Criteria {
        Criteria {
            version_type_id: self.version_type_id,
            game_version: self.game_version.clone(),
            channel: self.channel,
            allow_client_mismatch,
        }
    }

    /// The run targets another client than the profile's.
    pub fn is_foreign(&self, profile: &GameProfile) -> bool {
        self.client != profile.client
            || self
                .profile_version_type_id
                .is_some_and(|id| id != self.version_type_id)
    }
}

/// Resolve client, version type, game version and channel for this run.
pub async fn resolve(
    provider: &dyn AddonProvider,
    profile: &GameProfile,
    settings: &Settings,
    filter: &FilterArgs,
    out: &Output,
) -> Result<Resolved> {
    let client = filter.client.unwrap_or(profile.client);
    let types = provider.version_types().await?;
    let same_client = client == profile.client;
    let version_type_id = resolver::resolve_version_type(
        &types,
        client,
        filter.game_version_type_id,
        if same_client {
            profile.game_version_type_id
        } else {
            None
        },
        filter.client.is_some(),
    )?;
    let profile_version_type_id = resolver::resolve_version_type(
        &types,
        profile.client,
        None,
        profile.game_version_type_id,
        false,
    )
    .ok();

    let (game_version, from_cli) = match &filter.game_version {
        Some(v) => (Some(v.trim().to_string()), true),
        None => (profile.game_version.clone(), false),
    };
    if let Some(v) = &game_version {
        match provider.game_versions(version_type_id).await? {
            Some(list) if !list.iter().any(|g| g.trim() == v) => {
                let msg = format!("game version {v} does not belong to version type {version_type_id}");
                return Err(if from_cli { AppError::Usage(msg) } else { AppError::Config(msg) });
            }
            Some(_) => {}
            None => out.warn(&format!(
                "cannot list the game versions of type {version_type_id}; {v} is used without validation"
            )),
        }
    }
    let channel = filter
        .channel
        .or(profile.channel)
        .unwrap_or(settings.channel);
    Ok(Resolved {
        client,
        version_type_id,
        profile_version_type_id,
        game_version,
        channel,
    })
}

/// Result of checking one addon.
#[derive(Clone, Debug, Serialize)]
pub struct Evaluation {
    pub addon: String,
    pub folders: Vec<String>,
    pub project: Option<ProjectRef>,
    pub tracking: Tracking,
    pub installed: Option<String>,
    pub installed_file_id: Option<String>,
    pub status: CheckStatus,
    pub available: Option<RemoteFile>,
    pub message: Option<String>,
    #[serde(skip)]
    pub entry: AddonEntry,
}

impl Evaluation {
    fn new(entry: &AddonEntry, status: CheckStatus) -> Self {
        Evaluation {
            addon: entry.name.clone(),
            folders: entry.folders.clone(),
            project: entry.project.clone(),
            tracking: entry.tracking,
            installed: entry
                .local_version
                .clone()
                .or_else(|| entry.managed.as_ref().map(|m| m.display_version.clone())),
            installed_file_id: entry.managed.as_ref().and_then(|m| m.file_id.clone()),
            status,
            available: None,
            message: None,
            entry: entry.clone(),
        }
    }

    fn with(
        mut self,
        status: CheckStatus,
        file: Option<RemoteFile>,
        message: Option<String>,
    ) -> Self {
        self.status = status;
        self.available = file;
        self.message = message;
        self
    }
}

fn needs_remote(e: &AddonEntry) -> bool {
    matches!(
        e.tracking,
        Tracking::Managed | Tracking::Unmanaged | Tracking::Missing
    ) && e.project.is_some()
}

/// Check every entry. Authentication errors stop the run; other errors are per addon.
pub async fn evaluate(
    provider: &dyn AddonProvider,
    entries: &[AddonEntry],
    criteria: &Criteria,
) -> Result<Vec<Evaluation>> {
    let projects: Vec<ProjectRef> = entries
        .iter()
        .filter(|e| needs_remote(e))
        .filter_map(|e| e.project.clone())
        .collect();
    let remote: Result<HashMap<ProjectRef, RemoteProject>, ProviderError> = if projects.is_empty() {
        Ok(HashMap::new())
    } else {
        provider
            .get_projects(&projects)
            .await
            .map(|list| list.into_iter().map(|p| (p.project.clone(), p)).collect())
    };
    let remote = match remote {
        Err(e) if e.is_fatal() => return Err(e.into()),
        other => other,
    };

    let results: Vec<Result<Evaluation>> = stream::iter(entries.iter())
        .map(|entry| {
            let remote = &remote;
            async move { evaluate_one(provider, entry, remote, criteria).await }
        })
        .buffered(LOOKUP_CONCURRENCY)
        .collect()
        .await;
    results.into_iter().collect()
}

async fn evaluate_one(
    provider: &dyn AddonProvider,
    entry: &AddonEntry,
    remote: &Result<HashMap<ProjectRef, RemoteProject>, ProviderError>,
    criteria: &Criteria,
) -> Result<Evaluation> {
    let base = |status| Evaluation::new(entry, status);
    match entry.tracking {
        Tracking::Protected => return Ok(base(CheckStatus::Protected)),
        Tracking::Conflict => {
            return Ok(base(CheckStatus::Conflict).with(
                CheckStatus::Conflict,
                None,
                Some("TOC files disagree on the project ID".into()),
            ));
        }
        Tracking::Untracked => return Ok(base(CheckStatus::Untracked)),
        _ => {}
    }
    let Some(project) = &entry.project else {
        return Ok(base(CheckStatus::Untracked));
    };
    let remote = match remote {
        Ok(map) => map,
        Err(e) => {
            return Ok(base(CheckStatus::Error).with(
                CheckStatus::Error,
                None,
                Some(e.to_string()),
            ));
        }
    };
    let Some(info) = remote.get(project) else {
        return Ok(base(CheckStatus::Error).with(
            CheckStatus::Error,
            None,
            Some(format!("unknown project {project}")),
        ));
    };
    if !info.is_wow {
        return Ok(base(CheckStatus::Error).with(
            CheckStatus::Error,
            None,
            Some(format!("{project} is not a World of Warcraft project")),
        ));
    }
    // The author blocks third-party downloads: the file list is still read, so the
    // user learns whether an update exists and where to download it by hand.
    let blocked = info.allow_distribution == Some(false);
    let query = FileQuery {
        game_version_type_id: (!criteria.allow_client_mismatch).then_some(criteria.version_type_id),
        game_version: criteria.game_version.clone(),
    };
    let files = match provider.list_files(project, &query).await {
        Ok(f) => f,
        Err(e) if e.is_fatal() => return Err(e.into()),
        Err(_) if blocked => {
            return Ok(base(CheckStatus::DownloadNotAllowed).with(
                CheckStatus::DownloadNotAllowed,
                None,
                Some(manual_download_hint(info, None)),
            ));
        }
        Err(e) => {
            return Ok(base(CheckStatus::Error).with(
                CheckStatus::Error,
                None,
                Some(e.to_string()),
            ));
        }
    };
    let Some(file) = resolver::select_latest(&files, criteria).cloned() else {
        return Ok(base(CheckStatus::NoCompatibleRelease).with(
            CheckStatus::NoCompatibleRelease,
            None,
            None,
        ));
    };
    let status = match (&entry.managed, entry.tracking) {
        (_, Tracking::Missing) => CheckStatus::Missing,
        (Some(m), _) => match resolver::compare(&resolver::identify_installed(m, &files), &file) {
            Decision::Newer => CheckStatus::UpdateAvailable,
            Decision::Same | Decision::Older => CheckStatus::UpToDate,
        },
        (None, _) => CheckStatus::Unmanaged,
    };
    if blocked && status != CheckStatus::UpToDate {
        let hint = manual_download_hint(info, Some(&file));
        return Ok(base(CheckStatus::DownloadNotAllowed).with(
            CheckStatus::DownloadNotAllowed,
            Some(file),
            Some(hint),
        ));
    }
    Ok(base(status).with(status, Some(file), None))
}

/// Where to download a file by hand, and what to do with it.
pub fn manual_download_hint(info: &RemoteProject, file: Option<&RemoteFile>) -> String {
    let page = match (&info.website_url, file) {
        (Some(site), Some(f)) => format!("{site}/files/{}", f.id),
        (Some(site), None) => format!("{site}/files"),
        (None, _) => format!("https://www.curseforge.com/projects/{}", info.project.id),
    };
    format!(
        "the author does not allow third-party downloads; download it from {page},          save the ZIP in the inbox (`wowaddons inbox`), then run `wowaddons import`"
    )
}
