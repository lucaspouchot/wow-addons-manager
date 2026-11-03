//! End-to-end `check` / `update` / `rollback` against a fake CurseForge API.

mod common;

use serde_json::Value;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use common::{
    Env, FOREVER_TYPE, FakeCurseForge, RETAIL_TYPE, addon_zip, cf_file, make_addon, snapshot,
};

const BARTENDER: u64 = 111;
const OTHER: u64 = 333;

struct World {
    env: Env,
    cf: FakeCurseForge,
}

/// Bartender4 (unmanaged, project 111), CustomAddon (untracked),
/// RetailOnly (project 333, files for Retail only).
async fn world() -> World {
    let cf = FakeCurseForge::start().await;
    let mut env = Env::new();
    env.api_url = Some(cf.uri());
    env.setup_forever();
    make_addon(&env.addons, "Bartender4", "4.15.0", Some(BARTENDER));
    make_addon(&env.addons, "CustomAddon", "1.0.0", None);
    make_addon(&env.addons, "RetailOnly", "2.0", Some(OTHER));
    cf.projects(&[
        (BARTENDER, "Bartender4", 1, true),
        (OTHER, "RetailOnly", 1, true),
    ])
    .await;
    cf.files(
        OTHER,
        vec![cf_file(
            3001,
            OTHER,
            "2.1",
            "2026-10-01T00:00:00Z",
            &[RETAIL_TYPE],
            None,
        )],
    )
    .await;
    World { env, cf }
}

/// Publish a Bartender4 release and make it downloadable.
async fn publish_bartender(w: &World, files: &[(u64, &str, &str)]) {
    let mut list = Vec::new();
    for (id, version, date) in files {
        let bytes = addon_zip(&[
            ("Bartender4", version, Some(BARTENDER)),
            ("Bartender4_Options", version, None),
        ]);
        let f = cf_file(*id, BARTENDER, version, date, &[FOREVER_TYPE], Some(&bytes));
        w.cf.downloadable(&f, bytes).await;
        list.push(f);
    }
    w.cf.files(BARTENDER, list).await;
}

fn status_of<'a>(v: &'a Value, addon: &str) -> &'a str {
    v["result"]["addons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["addon"] == addon)
        .and_then(|a| a["status"].as_str())
        .unwrap_or("?")
}

#[tokio::test(flavor = "multi_thread")]
async fn check_changes_nothing() {
    let w = world().await;
    publish_bartender(&w, &[(1001, "4.16.0", "2026-10-20T00:00:00Z")]).await;
    let before = snapshot(&w.env.addons);
    let (code, v) = w.env.json(&["check"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(status_of(&v, "Bartender4"), "UNMANAGED");
    assert_eq!(status_of(&v, "CustomAddon"), "UNTRACKED");
    assert_eq!(status_of(&v, "RetailOnly"), "NO_COMPATIBLE_RELEASE");
    assert_eq!(v["result"]["version_type_id"], FOREVER_TYPE);
    assert_eq!(snapshot(&w.env.addons), before);
    assert!(!w.env.data.join("profiles/forever/state.json").exists());

    let (code, out, _) = w.env.run(&["check"]);
    assert_eq!(code, 0);
    assert!(out.contains("can be adopted"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn adopt_update_rollback() {
    let w = world().await;
    publish_bartender(&w, &[(1001, "4.16.0", "2026-10-20T00:00:00Z")]).await;
    let original = snapshot(&w.env.addons);

    // Unmanaged addons are not updated without --adopt.
    let (code, out, _) = w.env.run(&["update", "--yes"]);
    assert_eq!(code, 0);
    assert!(out.contains("Nothing to update"), "{out}");

    // Adoption.
    let (code, out, err) = w.env.run(&["update", "--adopt", "--yes"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("ADOPT"), "{out}");
    let state = w.env.state("forever");
    let rec = &state["projects"]["curseforge:111"];
    assert_eq!(rec["file_id"], "1001");
    assert_eq!(
        rec["folders"],
        serde_json::json!(["Bartender4", "Bartender4_Options"])
    );
    assert!(w.env.addons.join("Bartender4_Options").is_dir());
    assert!(w.env.addons.join("CustomAddon").is_dir());
    let (_, v) = w.env.json(&["check"]);
    assert_eq!(status_of(&v, "Bartender4"), "UP_TO_DATE");
    let after_adopt = snapshot(&w.env.addons);

    // A new release.
    w.cf.reset().await;
    w.cf.projects(&[
        (BARTENDER, "Bartender4", 1, true),
        (OTHER, "RetailOnly", 1, true),
    ])
    .await;
    w.cf.files(OTHER, vec![]).await;
    publish_bartender(
        &w,
        &[
            (1001, "4.16.0", "2026-10-20T00:00:00Z"),
            (1002, "4.17.0", "2026-10-30T00:00:00Z"),
        ],
    )
    .await;
    let (code, v) = w.env.json(&["check", "--fail-on-updates"]);
    assert_eq!(code, 10);
    assert_eq!(status_of(&v, "Bartender4"), "UPDATE_AVAILABLE");
    let (code, _, err) = w.env.run(&["update", "--yes"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        w.env.state("forever")["projects"]["curseforge:111"]["file_id"],
        "1002"
    );
    let (_, v) = w.env.json(&["check"]);
    assert_eq!(status_of(&v, "Bartender4"), "UP_TO_DATE");

    // History lists both.
    let (_, v) = w.env.json(&["history"]);
    let txs = v["result"]["transactions"].as_array().unwrap().clone();
    assert_eq!(txs.len(), 2);
    let (latest, first) = (
        txs[0]["id"].as_str().unwrap().to_string(),
        txs[1]["id"].as_str().unwrap().to_string(),
    );

    // Only the latest can be rolled back.
    let (code, _, err) = w.env.run(&["rollback", &first, "--yes"]);
    assert_eq!(code, 5, "{err}");
    let (code, _, err) = w.env.run(&["rollback", "--dry-run"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = w.env.run(&["rollback", &latest, "--yes"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(snapshot(&w.env.addons), after_adopt);
    assert_eq!(
        w.env.state("forever")["projects"]["curseforge:111"]["file_id"],
        "1001"
    );

    // Then the adoption itself: back to the original, unmanaged.
    let (code, _, err) = w.env.run(&["rollback", "--yes"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(snapshot(&w.env.addons), original);
    assert!(w.env.state("forever")["projects"]["curseforge:111"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn dry_run_changes_nothing() {
    let w = world().await;
    publish_bartender(&w, &[(1001, "4.16.0", "2026-10-20T00:00:00Z")]).await;
    let before = snapshot(&w.env.addons);
    let (code, out, err) = w.env.run(&["update", "--adopt", "--dry-run"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Dry run"), "{out}");
    assert!(
        out.contains("replace: Bartender4") && out.contains("add: Bartender4_Options"),
        "{out}"
    );
    assert_eq!(snapshot(&w.env.addons), before);
    assert!(!w.env.data.join("profiles/forever/state.json").exists());
    assert!(!w.env.data.join("profiles/forever/journal").exists());
    let work = w
        .env
        .addons
        .parent()
        .unwrap()
        .join(".wowaddons-work")
        .join("forever");
    assert!(
        std::fs::read_dir(&work)
            .map(|d| d.count() == 0)
            .unwrap_or(true)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_download_changes_nothing() {
    let w = world().await;
    let bytes = addon_zip(&[("Bartender4", "4.16.0", Some(BARTENDER))]);
    let mut f = cf_file(
        1001,
        BARTENDER,
        "4.16.0",
        "2026-10-20T00:00:00Z",
        &[FOREVER_TYPE],
        Some(&bytes),
    );
    f["hashes"] =
        serde_json::json!([{ "value": "0000000000000000000000000000000000000000", "algo": 1 }]);
    w.cf.downloadable(&f, bytes).await;
    w.cf.files(BARTENDER, vec![f]).await;
    let before = snapshot(&w.env.addons);
    let (code, out, err) = w.env.run(&["update", "--adopt", "--yes"]);
    assert_eq!(code, 6, "{out}\n{err}");
    assert!(out.contains("SHA-1"), "{out}");
    assert_eq!(snapshot(&w.env.addons), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn unsafe_archive_is_rejected() {
    let w = world().await;
    let bytes = common::zip_bytes(&[
        ("Bartender4/Bartender4.toc", b"## Title: B\n"),
        ("Bartender4/../../x", b"x"),
    ]);
    let f = cf_file(
        1001,
        BARTENDER,
        "4.16.0",
        "2026-10-20T00:00:00Z",
        &[FOREVER_TYPE],
        Some(&bytes),
    );
    w.cf.downloadable(&f, bytes).await;
    w.cf.files(BARTENDER, vec![f]).await;
    let before = snapshot(&w.env.addons);
    let (code, out, _) = w.env.run(&["update", "--adopt", "--yes"]);
    assert_eq!(code, 6);
    assert!(out.contains("archive rejected"), "{out}");
    assert_eq!(snapshot(&w.env.addons), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn folder_owned_by_another_project_is_a_conflict() {
    let w = world().await;
    // The archive also ships "CustomAddon", which is untracked: conflict for a managed update,
    // but here we adopt, so make it belong to another project instead.
    make_addon(&w.env.addons, "Shared", "1.0", Some(OTHER));
    let bytes = addon_zip(&[
        ("Bartender4", "4.16.0", Some(BARTENDER)),
        ("Shared", "9.9", None),
    ]);
    let f = cf_file(
        1001,
        BARTENDER,
        "4.16.0",
        "2026-10-20T00:00:00Z",
        &[FOREVER_TYPE],
        Some(&bytes),
    );
    w.cf.downloadable(&f, bytes).await;
    w.cf.files(BARTENDER, vec![f]).await;
    let before = snapshot(&w.env.addons);
    let (code, out, _) = w
        .env
        .run(&["update", "--adopt", "--addon", "Bartender4", "--yes"]);
    assert_eq!(code, 5, "{out}");
    assert!(out.contains("CONFLICT") && out.contains("Shared"), "{out}");
    assert_eq!(snapshot(&w.env.addons), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_and_version_overrides() {
    let w = world().await;
    publish_bartender(&w, &[(1001, "4.16.0", "2026-10-20T00:00:00Z")]).await;
    let config_before = std::fs::read(&w.env.config).unwrap();

    // One-off game version filter, sent to the API and not saved.
    Mock::given(method("GET"))
        .and(path("/v1/mods/111/files"))
        .and(query_param("gameVersion", "1.60.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [], "pagination": { "index": 0, "pageSize": 50, "resultCount": 0, "totalCount": 0 }
        })))
        .with_priority(1)
        .mount(&w.cf.server)
        .await;
    let (code, v) = w.env.json(&["check", "--game-version", "1.60.1"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["result"]["game_version"], "1.60.1");
    assert_eq!(status_of(&v, "Bartender4"), "NO_COMPATIBLE_RELEASE");
    assert_eq!(std::fs::read(&w.env.config).unwrap(), config_before);
    // Unknown game version for the client: usage error.
    assert_eq!(w.env.run(&["check", "--game-version", "11.2.5"]).0, 2);

    // --client retail on update is refused for a Forever profile.
    let (code, _, err) = w
        .env
        .run(&["update", "--client", "retail", "--adopt", "--yes"]);
    assert_eq!(code, 5, "{err}");
    assert!(err.contains("--allow-client-mismatch"), "{err}");
    // Mismatching --client and --game-version-type-id.
    assert_eq!(
        w.env
            .run(&[
                "check",
                "--client",
                "forever",
                "--game-version-type-id",
                "517"
            ])
            .0,
        2
    );
    // A retail check works and uses the retail version type.
    let (code, v) = w.env.json(&["check", "--client", "retail"]);
    assert_eq!(code, 0);
    assert_eq!(v["result"]["version_type_id"], RETAIL_TYPE);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_update_is_refused() {
    let w = world().await;
    publish_bartender(&w, &[(1001, "4.16.0", "2026-10-20T00:00:00Z")]).await;
    let lock = w.env.data.join("profiles").join("forever").join("lock");
    let held = wow_addons_manager::storage::FileLock::acquire(&lock, "forever").unwrap();
    let before = snapshot(&w.env.addons);
    let (code, _, err) = w.env.run(&["update", "--adopt", "--yes"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("another wowaddons process"), "{err}");
    assert_eq!(snapshot(&w.env.addons), before);
    drop(held);
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_recovery_through_the_cli() {
    use time::OffsetDateTime;
    use wow_addons_manager::domain::ProjectRef;
    use wow_addons_manager::installer::{
        Journal, JournalItem, TxContext, TxHooks, TxKind, TxStatus, execute,
    };
    use wow_addons_manager::storage::AppPaths;

    let w = world().await;
    publish_bartender(&w, &[(1001, "4.16.0", "2026-10-20T00:00:00Z")]).await;
    let before = snapshot(&w.env.addons);
    // Simulate a process killed in the middle of a transaction.
    let ctx = TxContext {
        paths: AppPaths {
            config_file: w.env.config.clone(),
            data_dir: w.env.data.clone(),
        },
        profile: "forever".parse().unwrap(),
        addons_path: std::fs::canonicalize(&w.env.addons).unwrap(),
        work_dir: w
            .env
            .addons
            .parent()
            .unwrap()
            .join(".wowaddons-work")
            .join("forever"),
        backup_enabled: true,
        backup_retention: 5,
    };
    let id = "20261103T120000Z-beef";
    let staging = ctx.tx_dir(id).join("staging").join("0");
    make_addon(&staging, "Bartender4", "9.9.9", Some(BARTENDER));
    let journal = Journal {
        schema_version: 1,
        id: id.into(),
        profile: "forever".into(),
        kind: TxKind::Update,
        status: TxStatus::Prepared,
        created_at: OffsetDateTime::now_utc(),
        finished_at: None,
        addons_path: ctx.addons_path.clone(),
        tx_dir: ctx.tx_dir(id),
        items: vec![JournalItem {
            project: ProjectRef::curseforge("111"),
            addon: "Bartender4".into(),
            action: "ADOPT".into(),
            display_from: None,
            display_to: None,
            staging,
            move_aside: vec!["Bartender4".into()],
            removed: vec![],
            install: vec!["Bartender4".into()],
            previous: None,
            next: None,
        }],
        steps: vec![],
        had_state: false,
        backup_dir: None,
        reverts: None,
        reverted_by: None,
        error: None,
        path: ctx.journal_path(id),
    };
    let hooks = TxHooks {
        fail_after_steps: Some(1),
        crash: true,
        ..TxHooks::default()
    };
    assert!(execute(&ctx, journal, &hooks).is_err());
    assert!(!w.env.addons.join("Bartender4").exists());

    let (_, v) = w.env.json(&["--offline", "doctor"]);
    assert!(
        v["result"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "profile forever transaction")
    );

    let (code, _, err) = w.env.run(&["update", "--yes"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("recovered interrupted transaction"), "{err}");
    assert_eq!(snapshot(&w.env.addons), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_error_stops_the_run() {
    let w = world().await;
    w.cf.server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&w.cf.server)
        .await;
    let (code, _, err) = w.env.run(&["check"]);
    assert_eq!(code, 3, "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn other_game_and_distribution_blocked() {
    let cf = FakeCurseForge::start().await;
    let mut env = Env::new();
    env.api_url = Some(cf.uri());
    env.setup_forever();
    make_addon(&env.addons, "Weird", "1", Some(500));
    make_addon(&env.addons, "Closed", "1", Some(600));
    cf.projects(&[
        (500, "A Minecraft mod", 432, true),
        (600, "Closed", 1, false),
    ])
    .await;
    let (code, v) = env.json(&["check"]);
    assert_eq!(code, 6, "{v}");
    assert_eq!(status_of(&v, "Weird"), "ERROR");
    assert_eq!(status_of(&v, "Closed"), "DOWNLOAD_NOT_ALLOWED");
}

#[tokio::test(flavor = "multi_thread")]
async fn update_without_compatible_release_for_named_addon_is_blocked() {
    let w = world().await;
    let (code, out, _) = w
        .env
        .run(&["update", "--adopt", "--addon", "RetailOnly", "--yes"]);
    assert_eq!(code, 5, "{out}");
    assert!(out.contains("NO_COMPATIBLE_RELEASE"), "{out}");
    // Untracked addons cannot be updated.
    assert_eq!(
        w.env.run(&["update", "--addon", "CustomAddon", "--yes"]).0,
        5
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_file_id_and_downgrade() {
    let w = world().await;
    publish_bartender(
        &w,
        &[
            (1001, "4.16.0", "2026-10-20T00:00:00Z"),
            (1002, "4.17.0", "2026-10-30T00:00:00Z"),
        ],
    )
    .await;
    assert_eq!(w.env.run(&["update", "--adopt", "--yes"]).0, 0);
    assert_eq!(
        w.env.state("forever")["projects"]["curseforge:111"]["file_id"],
        "1002"
    );
    // Same file: needs --reinstall.
    assert_eq!(
        w.env
            .run(&[
                "update",
                "--addon",
                "Bartender4",
                "--file-id",
                "1002",
                "--yes"
            ])
            .0,
        5
    );
    // Explicit older file: downgrade.
    let (code, out, err) = w.env.run(&[
        "update",
        "--addon",
        "Bartender4",
        "--file-id",
        "1001",
        "--yes",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("DOWNGRADE"), "{out}");
    assert_eq!(
        w.env.state("forever")["projects"]["curseforge:111"]["file_id"],
        "1001"
    );
    // A plain update goes forward again; reinstall repairs a missing folder.
    std::fs::remove_dir_all(w.env.addons.join("Bartender4_Options")).unwrap();
    let (_, v) = w.env.json(&["list"]);
    assert_eq!(v["result"]["addons"][0]["tracking"], "MISSING");
    let (code, _, err) = w.env.run(&["update", "--reinstall", "--yes"]);
    assert_eq!(code, 0, "{err}");
    assert!(w.env.addons.join("Bartender4_Options").is_dir());
}

#[tokio::test(flavor = "multi_thread")]
async fn json_update_output() {
    let w = world().await;
    publish_bartender(&w, &[(1001, "4.16.0", "2026-10-20T00:00:00Z")]).await;
    // JSON mode never prompts: --yes is required.
    let (code, v) = w.env.json(&["update", "--adopt"]);
    assert_eq!(code, 2, "{v}");
    let (code, v) = w.env.json(&["update", "--adopt", "--yes"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["ok"], true);
    assert_eq!(v["result"]["installed"][0]["action"], "ADOPT");
    assert!(v["result"]["transaction_id"].is_string());
}
