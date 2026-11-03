//! Remote addon sources. CurseForge is the first implementation.

pub mod curseforge;

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::domain::{ProjectRef, ProviderKind, RemoteFile};
pub use crate::net::NetError as ProviderError;

/// A game version type (one per WoW client on CurseForge).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VersionType {
    pub id: u32,
    pub name: String,
    pub slug: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RemoteProject {
    pub project: ProjectRef,
    pub name: String,
    /// The project belongs to World of Warcraft.
    pub is_wow: bool,
    /// `Some(false)` when the author blocks third-party downloads.
    pub allow_distribution: Option<bool>,
    pub is_available: bool,
    /// Project page, when the provider gives a safe one.
    pub website_url: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct FileQuery {
    pub game_version_type_id: Option<u32>,
    pub game_version: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DownloadedArchive {
    pub path: PathBuf,
    pub size: u64,
    /// Lowercase hex SHA-1 computed while downloading.
    pub sha1: String,
}

#[async_trait::async_trait]
pub trait AddonProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    async fn version_types(&self) -> Result<Vec<VersionType>, ProviderError>;

    /// Game versions known for a version type, or `None` if the provider cannot list them.
    async fn game_versions(
        &self,
        version_type_id: u32,
    ) -> Result<Option<Vec<String>>, ProviderError>;

    /// Projects that exist. Unknown IDs are left out of the result.
    async fn get_projects(&self, ids: &[ProjectRef]) -> Result<Vec<RemoteProject>, ProviderError>;

    async fn list_files(
        &self,
        project: &ProjectRef,
        query: &FileQuery,
    ) -> Result<Vec<RemoteFile>, ProviderError>;

    async fn get_file(
        &self,
        project: &ProjectRef,
        file_id: &str,
    ) -> Result<RemoteFile, ProviderError>;

    async fn download(
        &self,
        file: &RemoteFile,
        destination: &Path,
        max_bytes: u64,
        progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<DownloadedArchive, ProviderError>;
}
