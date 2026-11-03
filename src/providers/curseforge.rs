//! CurseForge backend.
//!
//! CurseForge facts (game ID, file status codes) are constants in this file.

use std::path::Path;

use reqwest::Url;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::{
    AddonProvider, DownloadedArchive, FileQuery, ProviderError, RemoteProject, VersionType,
};
use crate::domain::{ProjectRef, ProviderKind, ReleaseChannel, RemoteFile};
use crate::net::{ApiKey, HostAllowlist, HttpClient, NetConfig};

pub const DEFAULT_API_URL: &str = "https://api.curseforge.com";
/// Test-only override. Accepted only for loopback hosts so the key can never leave the machine.
pub const ENV_API_URL: &str = "WOWADDONS_CURSEFORGE_API_URL";
/// World of Warcraft on CurseForge.
pub const WOW_GAME_ID: u32 = 1;
const PAGE_SIZE: u32 = 50;
const MAX_INDEX: u32 = 10_000;
const BATCH_SIZE: usize = 50;
/// `fileStatus` values of published files: 4 = Approved, 10 = Released (CurseForge
/// `FileStatus` enum). Only 4 was seen on real WoW files (checked 2026-11-03).
const PUBLISHED_STATUSES: &[u32] = &[4, 10];
/// `hashes[].algo` value for SHA-1.
const HASH_ALGO_SHA1: u32 = 1;

pub struct CurseForgeProvider {
    http: HttpClient,
    base: Url,
}

impl CurseForgeProvider {
    pub fn new(key: ApiKey, cfg: NetConfig) -> Result<Self, ProviderError> {
        let base_str = std::env::var(ENV_API_URL)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_API_URL.to_string());
        Self::with_base_url(key, cfg, &base_str)
    }

    pub fn with_base_url(key: ApiKey, cfg: NetConfig, base: &str) -> Result<Self, ProviderError> {
        let base = Url::parse(base)
            .map_err(|e| ProviderError::InvalidResponse(format!("invalid API URL: {e}")))?;
        let host = base.host_str().unwrap_or_default().to_ascii_lowercase();
        let mut allowlist = HostAllowlist::curseforge();
        if base.as_str().trim_end_matches('/') != DEFAULT_API_URL {
            let loopback = matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]");
            if !loopback {
                return Err(ProviderError::HostNotAllowed(host));
            }
            allowlist = allowlist.with_host(&host);
        }
        Ok(CurseForgeProvider {
            http: HttpClient::new(cfg, Some(key), allowlist)?,
            base,
        })
    }

    fn url(&self, path: &str) -> Result<Url, ProviderError> {
        self.base
            .join(path)
            .map_err(|e| ProviderError::InvalidResponse(format!("invalid URL: {e}")))
    }
}

// --- DTOs ---------------------------------------------------------------

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Deserialize)]
struct Pagination {
    index: u32,
    #[serde(rename = "pageSize")]
    page_size: u32,
    #[serde(rename = "resultCount")]
    result_count: u32,
    #[serde(rename = "totalCount")]
    total_count: u32,
}

#[derive(Deserialize)]
struct Paged<T> {
    data: Vec<T>,
    pagination: Option<Pagination>,
}

#[derive(Deserialize)]
struct CfVersionType {
    id: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    slug: String,
}

#[derive(Deserialize)]
struct CfVersionsByType {
    #[serde(rename = "type")]
    type_id: u32,
    #[serde(default)]
    versions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfMod {
    id: u64,
    game_id: u32,
    #[serde(default)]
    name: String,
    allow_mod_distribution: Option<bool>,
    is_available: Option<bool>,
    links: Option<CfLinks>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfLinks {
    website_url: Option<String>,
}

#[derive(Deserialize)]
struct CfHash {
    value: String,
    algo: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfSortableVersion {
    game_version_type_id: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfFile {
    id: u64,
    mod_id: u64,
    #[serde(default)]
    is_available: bool,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    file_name: String,
    release_type: u8,
    file_status: u32,
    #[serde(default)]
    hashes: Vec<CfHash>,
    file_date: String,
    file_length: Option<u64>,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    sortable_game_versions: Vec<CfSortableVersion>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModsRequest {
    mod_ids: Vec<u64>,
}

impl CfFile {
    fn into_domain(self) -> Result<RemoteFile, ProviderError> {
        let channel = match self.release_type {
            1 => ReleaseChannel::Release,
            2 => ReleaseChannel::Beta,
            3 => ReleaseChannel::Alpha,
            other => {
                return Err(ProviderError::InvalidResponse(format!(
                    "file {}: unknown releaseType {other}",
                    self.id
                )));
            }
        };
        let published_at = OffsetDateTime::parse(&self.file_date, &Rfc3339).map_err(|e| {
            ProviderError::InvalidResponse(format!("file {}: invalid fileDate: {e}", self.id))
        })?;
        let mut type_ids: Vec<u32> = self
            .sortable_game_versions
            .iter()
            .filter_map(|v| v.game_version_type_id)
            .collect();
        type_ids.sort_unstable();
        type_ids.dedup();
        let sha1 = self
            .hashes
            .iter()
            .find(|h| h.algo == HASH_ALGO_SHA1)
            .map(|h| h.value.trim().to_ascii_lowercase())
            .filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()));
        Ok(RemoteFile {
            id: self.id.to_string(),
            project: ProjectRef::curseforge(self.mod_id.to_string()),
            display_name: if self.display_name.is_empty() {
                self.file_name.clone()
            } else {
                self.display_name
            },
            file_name: self.file_name,
            channel,
            game_version_type_ids: type_ids,
            game_versions: self.game_versions,
            published_at,
            size: self.file_length.filter(|&l| l > 0),
            sha1,
            is_available: self.is_available,
            is_published: PUBLISHED_STATUSES.contains(&self.file_status),
            download_allowed: true,
        })
    }
}

/// Keep a project page URL only if it is a plain `https://www.curseforge.com/` link.
/// It is printed to the terminal, so control characters and spaces are refused.
fn safe_website_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let ok = url.starts_with("https://www.curseforge.com/")
        && url.len() <= 300
        && url.bytes().all(|b| b.is_ascii_graphic());
    ok.then(|| url.to_string())
}

fn numeric_id(p: &ProjectRef) -> Result<u64, ProviderError> {
    if p.provider != ProviderKind::CurseForge {
        return Err(ProviderError::InvalidResponse(format!(
            "{p} is not a CurseForge project"
        )));
    }
    p.id.parse().map_err(|_| {
        ProviderError::InvalidResponse(format!("invalid CurseForge project ID {}", p.id))
    })
}

#[async_trait::async_trait]
impl AddonProvider for CurseForgeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::CurseForge
    }

    async fn version_types(&self) -> Result<Vec<VersionType>, ProviderError> {
        let url = self.url(&format!("/v1/games/{WOW_GAME_ID}/version-types"))?;
        let env: Envelope<Vec<CfVersionType>> = self.http.get_json(url).await?;
        Ok(env
            .data
            .into_iter()
            .map(|v| VersionType {
                id: v.id,
                name: v.name,
                slug: v.slug,
            })
            .collect())
    }

    async fn game_versions(
        &self,
        version_type_id: u32,
    ) -> Result<Option<Vec<String>>, ProviderError> {
        let url = self.url(&format!("/v1/games/{WOW_GAME_ID}/versions"))?;
        match self
            .http
            .get_json::<Envelope<Vec<CfVersionsByType>>>(url)
            .await
        {
            Ok(env) => Ok(Some(
                env.data
                    .into_iter()
                    .filter(|v| v.type_id == version_type_id)
                    .flat_map(|v| v.versions)
                    .collect(),
            )),
            Err(ProviderError::NotFound(_)) | Err(ProviderError::InvalidResponse(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn get_projects(&self, ids: &[ProjectRef]) -> Result<Vec<RemoteProject>, ProviderError> {
        let mut numeric = Vec::new();
        for p in ids {
            numeric.push(numeric_id(p)?);
        }
        numeric.sort_unstable();
        numeric.dedup();
        let mut out = Vec::new();
        for chunk in numeric.chunks(BATCH_SIZE) {
            let url = self.url("/v1/mods")?;
            let env: Envelope<Vec<CfMod>> = self
                .http
                .post_json(
                    url,
                    &ModsRequest {
                        mod_ids: chunk.to_vec(),
                    },
                )
                .await?;
            out.extend(env.data.into_iter().map(|m| {
                RemoteProject {
                    project: ProjectRef::curseforge(m.id.to_string()),
                    name: m.name,
                    is_wow: m.game_id == WOW_GAME_ID,
                    allow_distribution: m.allow_mod_distribution,
                    is_available: m.is_available.unwrap_or(true),
                    website_url: m
                        .links
                        .and_then(|l| l.website_url)
                        .and_then(|u| safe_website_url(&u)),
                }
            }));
        }
        Ok(out)
    }

    async fn list_files(
        &self,
        project: &ProjectRef,
        query: &FileQuery,
    ) -> Result<Vec<RemoteFile>, ProviderError> {
        let id = numeric_id(project)?;
        let mut index = 0u32;
        let mut out = Vec::new();
        loop {
            let mut url = self.url(&format!("/v1/mods/{id}/files"))?;
            {
                let mut q = url.query_pairs_mut();
                if let Some(t) = query.game_version_type_id {
                    q.append_pair("gameVersionTypeId", &t.to_string());
                }
                if let Some(v) = &query.game_version {
                    q.append_pair("gameVersion", v);
                }
                q.append_pair("index", &index.to_string());
                q.append_pair("pageSize", &PAGE_SIZE.to_string());
            }
            let page: Paged<CfFile> = self.http.get_json(url).await?;
            let count = page.data.len() as u32;
            for f in page.data {
                out.push(f.into_domain()?);
            }
            let Some(p) = page.pagination else { break };
            let next = p.index + p.page_size.max(1);
            if count == 0
                || p.result_count == 0
                || next >= p.total_count
                || next + PAGE_SIZE > MAX_INDEX
            {
                break;
            }
            index = next;
        }
        Ok(out)
    }

    async fn get_file(
        &self,
        project: &ProjectRef,
        file_id: &str,
    ) -> Result<RemoteFile, ProviderError> {
        let id = numeric_id(project)?;
        let fid: u64 = file_id
            .parse()
            .map_err(|_| ProviderError::InvalidResponse(format!("invalid file ID {file_id}")))?;
        let url = self.url(&format!("/v1/mods/{id}/files/{fid}"))?;
        let env: Envelope<CfFile> = self.http.get_json(url).await?;
        let file = env.data.into_domain()?;
        if file.project != *project {
            return Err(ProviderError::InvalidResponse(format!(
                "file {file_id} does not belong to {project}"
            )));
        }
        Ok(file)
    }

    async fn download(
        &self,
        file: &RemoteFile,
        destination: &Path,
        max_bytes: u64,
        progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<DownloadedArchive, ProviderError> {
        let id = numeric_id(&file.project)?;
        let url = self.url(&format!("/v1/mods/{id}/files/{}/download-url", file.id))?;
        let env: Envelope<Option<String>> = match self.http.get_json(url).await {
            Ok(env) => env,
            Err(ProviderError::NotFound(_)) => return Err(ProviderError::DownloadNotAllowed),
            Err(e) => return Err(e),
        };
        let Some(raw) = env.data.filter(|s| !s.trim().is_empty()) else {
            return Err(ProviderError::DownloadNotAllowed);
        };
        let download_url = Url::parse(raw.trim())
            .map_err(|e| ProviderError::InvalidResponse(format!("invalid download URL: {e}")))?;
        let (size, sha1) = self
            .http
            .download(download_url, destination, max_bytes, progress)
            .await?;
        Ok(DownloadedArchive {
            path: destination.to_path_buf(),
            size,
            sha1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::safe_website_url;

    #[test]
    fn website_urls() {
        assert_eq!(
            safe_website_url("https://www.curseforge.com/wow/addons/details/").as_deref(),
            Some("https://www.curseforge.com/wow/addons/details")
        );
        assert!(safe_website_url("https://evil.example/wow").is_none());
        assert!(safe_website_url("http://www.curseforge.com/wow").is_none());
        assert!(safe_website_url("https://www.curseforge.com/a b").is_none());
        assert!(safe_website_url("https://www.curseforge.com/\u{1b}[2J").is_none());
    }
}
