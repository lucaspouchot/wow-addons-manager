//! Archive safety and transactions at library level.

mod common;

use std::path::{Path, PathBuf};

use time::OffsetDateTime;

use common::{addon_zip, make_addon, snapshot, zip_bytes};
use wow_addons_manager::domain::{ProfileName, ProjectRef, WowClient};
use wow_addons_manager::error::AppError;
use wow_addons_manager::installer::{
    ArchiveLimits, Journal, JournalItem, TxContext, TxHooks, TxKind, TxStatus, execute, extract,
    list_journals, recover,
};
use wow_addons_manager::storage::{AppPaths, FileLock, ManagedProject, State};

const LIMITS: ArchiveLimits = ArchiveLimits {
    max_entries: 1000,
    max_extracted_bytes: 10 * 1024 * 1024,
};

fn write_zip(dir: &Path, bytes: &[u8]) -> PathBuf {
    let p = dir.join("a.zip");
    std::fs::write(&p, bytes).unwrap();
    p
}

fn try_extract(bytes: Vec<u8>) -> Result<Vec<String>, String> {
    let dir = tempfile::tempdir().unwrap();
    let zip = write_zip(dir.path(), &bytes);
    extract(&zip, &dir.path().join("out"), LIMITS)
        .map(|x| x.folders)
        .map_err(|e| e.to_string())
}

#[test]
fn extracts_one_and_several_folders() {
    assert_eq!(
        try_extract(addon_zip(&[("Example", "1", Some(1))])).unwrap(),
        vec!["Example"]
    );
    let got = try_extract(addon_zip(&[
        ("Example", "1", Some(1)),
        ("Example_Config", "1", None),
        ("Example_Options", "1", None),
    ]))
    .unwrap();
    assert_eq!(got, vec!["Example", "Example_Config", "Example_Options"]);
}

#[test]
fn rejects_unsafe_archives() {
    let toc = b"## Title: A\n".as_slice();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "zip slip",
            zip_bytes(&[("A/A.toc", toc), ("A/../../evil.txt", b"x")]),
        ),
        (
            "absolute",
            zip_bytes(&[("A/A.toc", toc), ("/etc/evil", b"x")]),
        ),
        (
            "root file",
            zip_bytes(&[("A/A.toc", toc), ("readme.txt", b"x")]),
        ),
        (
            "case clash",
            zip_bytes(&[("A/A.toc", toc), ("A/x.lua", b"1"), ("A/X.lua", b"2")]),
        ),
        (
            "reserved",
            zip_bytes(&[("A/A.toc", toc), ("A/CON.lua", b"x")]),
        ),
        ("no toc", zip_bytes(&[("A/a.lua", b"x")])),
        ("not a zip", b"definitely not a zip".to_vec()),
    ];
    for (name, bytes) in cases {
        assert!(try_extract(bytes).is_err(), "{name} should be rejected");
    }
}

#[test]
fn rejects_symlinks() {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        w.start_file("A/A.toc", o).unwrap();
        w.write_all(b"## Title: A\n").unwrap();
        w.add_symlink("A/link", "/etc/passwd", o).unwrap();
        w.finish().unwrap();
    }
    let err = try_extract(buf.into_inner()).unwrap_err();
    assert!(err.contains("symbolic link"), "{err}");
}

#[test]
fn enforces_limits() {
    let big = vec![0u8; 2 * 1024 * 1024];
    let bytes = zip_bytes(&[("A/A.toc", b"## Title: A\n"), ("A/big.bin", &big)]);
    let dir = tempfile::tempdir().unwrap();
    let zip = write_zip(dir.path(), &bytes);
    let small = ArchiveLimits {
        max_entries: 1000,
        max_extracted_bytes: 1024 * 1024,
    };
    assert!(
        extract(&zip, &dir.path().join("o1"), small)
            .unwrap_err()
            .to_string()
            .contains("limit")
    );
    let few = ArchiveLimits {
        max_entries: 1,
        max_extracted_bytes: 1 << 30,
    };
    assert!(
        extract(&zip, &dir.path().join("o2"), few)
            .unwrap_err()
            .to_string()
            .contains("entries")
    );
}

// --- transactions -----------------------------------------------------------

struct Fixture {
    _dir: tempfile::TempDir,
    ctx: TxContext,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let addons = dir.path().join("Interface").join("AddOns");
    std::fs::create_dir_all(&addons).unwrap();
    let paths = AppPaths {
        config_file: dir.path().join("config.toml"),
        data_dir: dir.path().join("data"),
    };
    let profile: ProfileName = "forever".parse().unwrap();
    let ctx = TxContext {
        paths,
        work_dir: dir
            .path()
            .join("Interface")
            .join(".wowaddons-work")
            .join("forever"),
        profile,
        addons_path: addons,
        backup_enabled: true,
        backup_retention: 2,
    };
    Fixture { _dir: dir, ctx }
}

fn record(file_id: &str, folders: &[&str], tx: &str) -> ManagedProject {
    ManagedProject {
        file_id: Some(file_id.into()),
        archive_sha1: None,
        display_version: format!("v{file_id}"),
        file_date: OffsetDateTime::now_utc(),
        folders: folders.iter().map(|s| s.to_string()).collect(),
        client: WowClient::Forever,
        game_version_type_id: Some(88568),
        client_mismatch: false,
        installed_at: OffsetDateTime::now_utc(),
        transaction_id: tx.into(),
    }
}

/// Prepare a journal that replaces `Example` (v1) with `Example` + `Example_Config` (v2)
/// and removes `Example_Old`.
fn prepare(f: &Fixture, id: &str) -> Journal {
    let tx_dir = f.ctx.tx_dir(id);
    let staging = tx_dir.join("staging").join("0");
    make_addon(&staging, "Example", "2.0", Some(1));
    make_addon(&staging, "Example_Config", "2.0", None);
    Journal {
        schema_version: 1,
        id: id.into(),
        profile: "forever".into(),
        kind: TxKind::Update,
        status: TxStatus::Prepared,
        created_at: OffsetDateTime::now_utc(),
        finished_at: None,
        addons_path: f.ctx.addons_path.clone(),
        tx_dir,
        items: vec![JournalItem {
            project: Some(ProjectRef::curseforge("1")),
            addon: "Example".into(),
            action: "UPDATE".into(),
            display_from: Some("v1".into()),
            display_to: Some("v2".into()),
            staging,
            move_aside: vec!["Example".into(), "Example_Old".into()],
            removed: vec!["Example_Old".into()],
            install: vec!["Example".into(), "Example_Config".into()],
            previous: Some(record("1", &["Example", "Example_Old"], "t0")),
            next: Some(record("2", &["Example", "Example_Config"], id)),
        }],
        steps: Vec::new(),
        had_state: false,
        backup_dir: None,
        reverts: None,
        reverted_by: None,
        error: None,
        path: f.ctx.journal_path(id),
    }
}

/// Install v1 + an unrelated addon, and the matching state.
fn seed(f: &Fixture) {
    make_addon(&f.ctx.addons_path, "Example", "1.0", Some(1));
    make_addon(&f.ctx.addons_path, "Example_Old", "1.0", None);
    make_addon(&f.ctx.addons_path, "Other", "3.0", Some(9));
    let mut state = State::new("forever");
    state.projects.insert(
        ProjectRef::curseforge("1"),
        record("1", &["Example", "Example_Old"], "t0"),
    );
    state.save(&f.ctx.state_path()).unwrap();
}

#[test]
fn successful_transaction_replaces_removes_and_backs_up() {
    let f = fixture();
    seed(&f);
    let outcome = execute(
        &f.ctx,
        prepare(&f, "20261103T000001Z-0001"),
        &TxHooks::default(),
    )
    .unwrap();
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    let a = &f.ctx.addons_path;
    assert!(
        std::fs::read_to_string(a.join("Example/Example.toc"))
            .unwrap()
            .contains("2.0")
    );
    assert!(a.join("Example_Config").is_dir());
    assert!(!a.join("Example_Old").exists());
    assert!(a.join("Other").is_dir());
    let state = State::load(&f.ctx.state_path(), "forever").unwrap();
    assert_eq!(
        state.projects[&ProjectRef::curseforge("1")]
            .file_id
            .as_deref(),
        Some("2")
    );
    let backup = outcome.journal.backup_dir.unwrap();
    assert!(backup.join("folders/Example/Example.toc").exists());
    assert!(backup.join("folders/Example_Old").is_dir());
    assert!(backup.join("manifest.json").exists());
    assert!(!f.ctx.tx_dir("20261103T000001Z-0001").exists());
    assert_eq!(outcome.journal.status, TxStatus::Finalized);
}

#[test]
fn failure_at_every_step_restores_everything() {
    // 4 steps: 2 move-asides, 2 installs. Fail after 0..=4 of them, and before commit.
    for fail_after in 0..=5 {
        let f = fixture();
        seed(&f);
        let before_addons = snapshot(&f.ctx.addons_path);
        let before_state = std::fs::read(f.ctx.state_path()).unwrap();
        let hooks = if fail_after == 5 {
            TxHooks {
                fail_before_commit: true,
                ..TxHooks::default()
            }
        } else {
            TxHooks {
                fail_after_steps: Some(fail_after),
                ..TxHooks::default()
            }
        };
        let err = execute(&f.ctx, prepare(&f, "20261103T000002Z-0002"), &hooks).unwrap_err();
        assert!(
            matches!(err, AppError::InstallFailed(_)),
            "step {fail_after}: {err}"
        );
        assert_eq!(
            snapshot(&f.ctx.addons_path),
            before_addons,
            "step {fail_after}"
        );
        assert_eq!(
            std::fs::read(f.ctx.state_path()).unwrap(),
            before_state,
            "step {fail_after}"
        );
        let j = &list_journals(&f.ctx.journal_dir()).unwrap()[0];
        assert_eq!(j.status, TxStatus::RolledBack);
    }
}

#[test]
fn recovers_an_interrupted_transaction() {
    for crash_after in 0..=4 {
        let f = fixture();
        seed(&f);
        let before_addons = snapshot(&f.ctx.addons_path);
        let before_state = std::fs::read(f.ctx.state_path()).unwrap();
        let hooks = TxHooks {
            fail_after_steps: Some(crash_after),
            crash: true,
            ..TxHooks::default()
        };
        assert!(execute(&f.ctx, prepare(&f, "20261103T000003Z-0003"), &hooks).is_err());
        assert_eq!(
            list_journals(&f.ctx.journal_dir()).unwrap()[0].status,
            TxStatus::Prepared
        );

        let messages = recover(&f.ctx).unwrap();
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert_eq!(
            snapshot(&f.ctx.addons_path),
            before_addons,
            "crash after {crash_after}"
        );
        assert_eq!(std::fs::read(f.ctx.state_path()).unwrap(), before_state);
        assert_eq!(
            list_journals(&f.ctx.journal_dir()).unwrap()[0].status,
            TxStatus::RolledBack
        );
        assert!(recover(&f.ctx).unwrap().is_empty());
    }
}

#[test]
fn recovery_finishes_a_committed_transaction() {
    let f = fixture();
    seed(&f);
    let mut j = execute(
        &f.ctx,
        prepare(&f, "20261103T000004Z-0004"),
        &TxHooks::default(),
    )
    .unwrap()
    .journal;
    // Pretend the process died right after the commit.
    j.status = TxStatus::Committed;
    j.save().unwrap();
    let msgs = recover(&f.ctx).unwrap();
    assert!(msgs.iter().any(|m| m.contains("post-commit")), "{msgs:?}");
    assert_eq!(
        list_journals(&f.ctx.journal_dir()).unwrap()[0].status,
        TxStatus::Finalized
    );
}

#[test]
fn backup_retention() {
    let f = fixture();
    for i in 0..4 {
        let dir = f.ctx.backups_dir().join(format!("2026110{i}T000000Z-0000"));
        std::fs::create_dir_all(dir.join("folders")).unwrap();
    }
    seed(&f);
    execute(
        &f.ctx,
        prepare(&f, "20261110T000000Z-0005"),
        &TxHooks::default(),
    )
    .unwrap();
    let kept: Vec<_> = std::fs::read_dir(f.ctx.backups_dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert!(f.ctx.backups_dir().join("20261110T000000Z-0005").exists());
}

#[test]
fn profile_lock_is_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("lock");
    let first = FileLock::acquire(&p, "forever").unwrap();
    let err = FileLock::acquire(&p, "forever").unwrap_err();
    assert!(matches!(err, AppError::Locked(_)));
    drop(first);
    assert!(FileLock::acquire(&p, "forever").is_ok());
}
