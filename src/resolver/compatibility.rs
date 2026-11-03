//! File eligibility and selection. Pure functions, no I/O.

use std::cmp::Ordering;

use serde::Serialize;

use crate::domain::release::cmp_file_ids;
use crate::domain::{ReleaseChannel, RemoteFile};
use crate::storage::ManagedProject;

#[derive(Clone, Debug)]
pub struct Criteria {
    pub version_type_id: u32,
    pub game_version: Option<String>,
    pub channel: ReleaseChannel,
    pub allow_client_mismatch: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ineligible {
    WrongClient,
    WrongGameVersion,
    Channel,
    Unavailable,
    Unpublished,
    DownloadNotAllowed,
}

impl Ineligible {
    pub fn describe(self) -> &'static str {
        match self {
            Ineligible::WrongClient => "the file is not tagged for this client",
            Ineligible::WrongGameVersion => "the file is not tagged for this game version",
            Ineligible::Channel => "the file's release type is not allowed by the channel",
            Ineligible::Unavailable => "the file is not available",
            Ineligible::Unpublished => "the file is not published",
            Ineligible::DownloadNotAllowed => "the author does not allow third-party downloads",
        }
    }
}

/// Is the file tagged for the client of the criteria?
pub fn matches_client(file: &RemoteFile, c: &Criteria) -> bool {
    file.game_version_type_ids.contains(&c.version_type_id)
}

pub fn eligibility(file: &RemoteFile, c: &Criteria) -> Result<(), Ineligible> {
    if !c.allow_client_mismatch && !matches_client(file, c) {
        return Err(Ineligible::WrongClient);
    }
    if let Some(v) = &c.game_version
        && !file.game_versions.iter().any(|g| g.trim() == v.trim())
    {
        return Err(Ineligible::WrongGameVersion);
    }
    if !c.channel.accepts(file.channel) {
        return Err(Ineligible::Channel);
    }
    if !file.is_available {
        return Err(Ineligible::Unavailable);
    }
    if !file.is_published {
        return Err(Ineligible::Unpublished);
    }
    if !file.download_allowed {
        return Err(Ineligible::DownloadNotAllowed);
    }
    Ok(())
}

/// Newest first: publication date, then file ID.
fn newer(a: &RemoteFile, b: &RemoteFile) -> Ordering {
    a.published_at
        .cmp(&b.published_at)
        .then_with(|| cmp_file_ids(&a.id, &b.id))
}

/// The most recent eligible file. With `allow_client_mismatch`, files tagged for
/// the client are still preferred over others.
pub fn select_latest<'a>(files: &'a [RemoteFile], c: &Criteria) -> Option<&'a RemoteFile> {
    let eligible = files.iter().filter(|f| eligibility(f, c).is_ok());
    let strict = c.allow_client_mismatch.then(|| {
        files
            .iter()
            .filter(|f| eligibility(f, c).is_ok() && matches_client(f, c))
            .max_by(|a, b| newer(a, b))
    });
    match strict.flatten() {
        Some(f) => Some(f),
        None => eligible.max_by(|a, b| newer(a, b)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Same file as installed.
    Same,
    /// Newer than the installed file.
    Newer,
    /// Older than the installed file: never applied automatically.
    Older,
}

/// Same archive bytes: both SHA-1 hashes are known and equal.
fn same_archive(installed: &ManagedProject, file: &RemoteFile) -> bool {
    matches!((&installed.archive_sha1, &file.sha1), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b))
}

/// For an archive imported from the inbox, find the remote file it is (by SHA-1)
/// and return the installed record completed with that file's ID and date.
/// Other records are returned unchanged.
pub fn identify_installed(installed: &ManagedProject, files: &[RemoteFile]) -> ManagedProject {
    let mut out = installed.clone();
    if out.file_id.is_none()
        && let Some(f) = files.iter().find(|f| same_archive(installed, f))
    {
        out.file_id = Some(f.id.clone());
        out.file_date = f.published_at;
    }
    out
}

/// Compare a candidate with the installed file using IDs, hashes and dates, never
/// version strings.
///
/// An imported archive whose remote file is unknown is only older than files
/// published after its import; anything else counts as `Older`, so it is never
/// replaced automatically.
pub fn compare(installed: &ManagedProject, candidate: &RemoteFile) -> Decision {
    let Some(installed_id) = &installed.file_id else {
        if same_archive(installed, candidate) {
            return Decision::Same;
        }
        return if candidate.published_at > installed.file_date {
            Decision::Newer
        } else {
            Decision::Older
        };
    };
    if *installed_id == candidate.id {
        return Decision::Same;
    }
    match candidate
        .published_at
        .cmp(&installed.file_date)
        .then_with(|| cmp_file_ids(&candidate.id, installed_id))
    {
        Ordering::Greater => Decision::Newer,
        Ordering::Less => Decision::Older,
        Ordering::Equal => Decision::Same,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ProjectRef, WowClient};
    use time::macros::datetime;

    fn file(id: &str, ch: ReleaseChannel, types: &[u32], date: time::OffsetDateTime) -> RemoteFile {
        RemoteFile {
            id: id.into(),
            project: ProjectRef::curseforge("1"),
            display_name: format!("v{id}"),
            file_name: format!("a-{id}.zip"),
            channel: ch,
            game_version_type_ids: types.to_vec(),
            game_versions: vec!["1.60.1".into()],
            published_at: date,
            size: None,
            sha1: None,
            is_available: true,
            is_published: true,
            download_allowed: true,
        }
    }

    fn crit(ch: ReleaseChannel) -> Criteria {
        Criteria {
            version_type_id: 88568,
            game_version: None,
            channel: ch,
            allow_client_mismatch: false,
        }
    }

    #[test]
    fn channels() {
        use ReleaseChannel::*;
        let files = vec![
            file("1", Release, &[88568], datetime!(2026-01-01 0:00 UTC)),
            file("2", Beta, &[88568], datetime!(2026-02-01 0:00 UTC)),
            file("3", Alpha, &[88568], datetime!(2026-03-01 0:00 UTC)),
        ];
        assert_eq!(select_latest(&files, &crit(Release)).unwrap().id, "1");
        assert_eq!(select_latest(&files, &crit(Beta)).unwrap().id, "2");
        assert_eq!(select_latest(&files, &crit(Alpha)).unwrap().id, "3");
    }

    #[test]
    fn never_another_client() {
        use ReleaseChannel::*;
        let files = vec![file("1", Release, &[517], datetime!(2026-01-01 0:00 UTC))];
        assert!(select_latest(&files, &crit(Release)).is_none());
        let mut c = crit(Release);
        c.allow_client_mismatch = true;
        assert_eq!(select_latest(&files, &c).unwrap().id, "1");
    }

    #[test]
    fn mismatch_still_prefers_client_files() {
        use ReleaseChannel::*;
        let files = vec![
            file("1", Release, &[88568], datetime!(2026-01-01 0:00 UTC)),
            file("2", Release, &[517], datetime!(2026-05-01 0:00 UTC)),
        ];
        let mut c = crit(Release);
        c.allow_client_mismatch = true;
        assert_eq!(select_latest(&files, &c).unwrap().id, "1");
    }

    #[test]
    fn game_version_filter() {
        let files = vec![file(
            "1",
            ReleaseChannel::Release,
            &[88568],
            datetime!(2026-01-01 0:00 UTC),
        )];
        let mut c = crit(ReleaseChannel::Release);
        c.game_version = Some("1.60.2".into());
        assert!(select_latest(&files, &c).is_none());
        c.game_version = Some("1.60.1".into());
        assert!(select_latest(&files, &c).is_some());
    }

    #[test]
    fn unpublished_and_unavailable() {
        let mut f = file(
            "1",
            ReleaseChannel::Release,
            &[88568],
            datetime!(2026-01-01 0:00 UTC),
        );
        f.is_published = false;
        assert_eq!(
            eligibility(&f, &crit(ReleaseChannel::Release)),
            Err(Ineligible::Unpublished)
        );
        f.is_published = true;
        f.is_available = false;
        assert_eq!(
            eligibility(&f, &crit(ReleaseChannel::Release)),
            Err(Ineligible::Unavailable)
        );
    }

    #[test]
    fn version_strings_are_ignored() {
        // "10.0" < "9.0" as strings, but dates decide.
        let mut old = file(
            "5",
            ReleaseChannel::Release,
            &[88568],
            datetime!(2026-01-01 0:00 UTC),
        );
        old.display_name = "10.0".into();
        let mut new = file(
            "6",
            ReleaseChannel::Release,
            &[88568],
            datetime!(2026-02-01 0:00 UTC),
        );
        new.display_name = "9.0".into();
        let files = vec![old, new];
        assert_eq!(
            select_latest(&files, &crit(ReleaseChannel::Release))
                .unwrap()
                .id,
            "6"
        );
    }

    #[test]
    fn no_downgrade() {
        let installed = ManagedProject {
            file_id: Some("20".into()),
            archive_sha1: None,
            display_version: "2".into(),
            file_date: datetime!(2026-03-01 0:00 UTC),
            folders: vec!["A".into()],
            client: WowClient::Forever,
            game_version_type_id: Some(88568),
            client_mismatch: false,
            installed_at: datetime!(2026-03-02 0:00 UTC),
            transaction_id: "t".into(),
        };
        let older = file(
            "10",
            ReleaseChannel::Release,
            &[88568],
            datetime!(2026-01-01 0:00 UTC),
        );
        let newer = file(
            "30",
            ReleaseChannel::Release,
            &[88568],
            datetime!(2026-04-01 0:00 UTC),
        );
        let same = file(
            "20",
            ReleaseChannel::Release,
            &[88568],
            datetime!(2026-03-01 0:00 UTC),
        );
        assert_eq!(compare(&installed, &older), Decision::Older);
        assert_eq!(compare(&installed, &newer), Decision::Newer);
        assert_eq!(compare(&installed, &same), Decision::Same);
    }

    #[test]
    fn imported_archive() {
        let mut installed = ManagedProject {
            file_id: None,
            archive_sha1: Some("aa".repeat(20)),
            display_version: "2".into(),
            file_date: datetime!(2026-03-02 0:00 UTC),
            folders: vec!["A".into()],
            client: WowClient::Forever,
            game_version_type_id: None,
            client_mismatch: false,
            installed_at: datetime!(2026-03-02 0:00 UTC),
            transaction_id: "t".into(),
        };
        let mut same = file(
            "20",
            ReleaseChannel::Release,
            &[88568],
            datetime!(2026-02-01 0:00 UTC),
        );
        same.sha1 = Some("AA".repeat(20));
        let before = file(
            "25",
            ReleaseChannel::Release,
            &[88568],
            datetime!(2026-02-15 0:00 UTC),
        );
        let after = file(
            "30",
            ReleaseChannel::Release,
            &[88568],
            datetime!(2026-04-01 0:00 UTC),
        );
        // Unknown remote file: only files published after the import are newer.
        assert_eq!(compare(&installed, &same), Decision::Same);
        assert_eq!(compare(&installed, &before), Decision::Older);
        assert_eq!(compare(&installed, &after), Decision::Newer);

        // Identified by SHA-1: exact comparison from then on.
        let files = vec![same.clone(), before.clone(), after.clone()];
        let known = identify_installed(&installed, &files);
        assert_eq!(known.file_id.as_deref(), Some("20"));
        assert_eq!(compare(&known, &before), Decision::Newer);

        installed.archive_sha1 = Some("bb".repeat(20));
        assert!(identify_installed(&installed, &files).file_id.is_none());
    }
}
