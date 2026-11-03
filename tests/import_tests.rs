//! `inbox` / `import`: installing ZIP files downloaded by hand, offline, and how
//! `check` follows them afterwards.

mod common;

use std::path::PathBuf;

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use common::{
    Env, FOREVER_TYPE, FakeCurseForge, addon_zip, cf_file, make_addon, sha1_hex, snapshot,
};

const CLOSED: u64 = 600;

fn inbox(env: &Env) -> PathBuf {
    env.data.join("profiles").join("forever").join("inbox")
}

fn drop_zip(env: &Env, name: &str, bytes: &[u8]) {
    std::fs::create_dir_all(inbox(env)).unwrap();
    std::fs::write(inbox(env).join(name), bytes).unwrap();
}

fn closed_zip(version: &str) -> Vec<u8> {
    addon_zip(&[
        ("Closed", version, Some(CLOSED)),
        ("Closed_Options", version, None),
    ])
}

fn skipped_status(v: &Value, archive: &str) -> String {
    v["result"]["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["archive"] == archive)
        .and_then(|s| s["status"].as_str())
        .unwrap_or("?")
        .to_string()
}

fn addon<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["result"]["addons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["addon"] == name)
        .expect("addon in check output")
}

/// `POST /v1/mods` for a project that blocks third-party downloads.
async fn closed_project(cf: &FakeCurseForge) {
    Mock::given(method("POST"))
        .and(path("/v1/mods"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [{
            "id": CLOSED, "gameId": 1, "name": "Closed",
            "allowModDistribution": false, "isAvailable": true,
            "links": { "websiteUrl": "https://www.curseforge.com/wow/addons/closed" },
        }]})))
        .mount(&cf.server)
        .await;
}

#[test]
fn inbox_is_created_per_profile() {
    let env = Env::new();
    env.setup_forever();
    let (code, v) = env.json(&["--offline", "inbox", "--no-open"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["result"]["opened"], false);
    assert_eq!(v["result"]["archives"], json!([]));
    assert!(inbox(&env).is_dir());

    drop_zip(&env, "Closed-2.0.zip", &closed_zip("2.0"));
    std::fs::write(inbox(&env).join("notes.txt"), "ignored").unwrap();
    let (_, out, _) = env.run(&["inbox", "--no-open"]);
    assert!(out.contains("1 ZIP file waiting: Closed-2.0.zip"), "{out}");
    assert!(out.contains("wowaddons import"), "{out}");

    // An empty inbox is not an error.
    std::fs::remove_file(inbox(&env).join("Closed-2.0.zip")).unwrap();
    let (code, out, _) = env.run(&["import"]);
    assert_eq!(code, 0);
    assert!(out.contains("The inbox is empty"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn import_offline_then_check_follows_the_file() {
    let cf = FakeCurseForge::start().await;
    let mut env = Env::new();
    env.api_url = Some(cf.uri());
    env.setup_forever();
    make_addon(&env.addons, "Closed", "1.0", Some(CLOSED));
    make_addon(&env.addons, "Other", "1.0", None);
    let v2 = closed_zip("2.0");
    drop_zip(&env, "Closed-2.0.zip", &v2);

    // Offline and without an API key.
    env.api_key = false;
    let before = snapshot(&env.addons);
    let (code, v) = env.json(&["--offline", "import", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["result"]["plan"][0]["addon"], "Closed");
    assert_eq!(v["result"]["plan"][0]["adopt"], true);
    assert_eq!(v["result"]["plan"][0]["add"], json!(["Closed_Options"]));
    assert_eq!(snapshot(&env.addons), before);
    assert!(inbox(&env).join("Closed-2.0.zip").exists());

    // Without a terminal, confirmation needs --yes.
    let (code, _, _) = env.run(&["--offline", "import"]);
    assert_eq!(code, 2);

    let (code, out, err) = env.run(&["--offline", "import", "--yes"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("IMPORT"), "{out}");
    assert!(out.contains("Installed 1 addon from the inbox"), "{out}");
    let toc = std::fs::read_to_string(env.addons.join("Closed/Closed.toc")).unwrap();
    assert!(toc.contains("2.0"));
    assert!(env.addons.join("Closed_Options").is_dir());
    assert!(env.addons.join("Other").is_dir());
    assert!(!inbox(&env).join("Closed-2.0.zip").exists());
    let rec = &env.state("forever")["projects"]["curseforge:600"];
    assert!(rec.get("file_id").is_none(), "{rec}");
    assert_eq!(rec["archive_sha1"], sha1_hex(&v2));
    assert_eq!(rec["display_version"], "2.0");
    assert_eq!(rec["folders"], json!(["Closed", "Closed_Options"]));

    // `check` finds the exact remote file through the archive's SHA-1.
    env.api_key = true;
    closed_project(&cf).await;
    let f2 = cf_file(
        6001,
        CLOSED,
        "2.0",
        "2026-10-01T00:00:00Z",
        &[FOREVER_TYPE],
        Some(&v2),
    );
    cf.files(CLOSED, vec![f2.clone()]).await;
    let (code, v) = env.json(&["check"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(addon(&v, "Closed")["status"], "UP_TO_DATE");

    // A newer file: manual download with a direct link, no download attempt.
    cf.reset().await;
    closed_project(&cf).await;
    let v3 = closed_zip("3.0");
    let f3 = cf_file(
        6002,
        CLOSED,
        "3.0",
        "2026-10-20T00:00:00Z",
        &[FOREVER_TYPE],
        Some(&v3),
    );
    cf.files(CLOSED, vec![f2, f3]).await;
    let (code, v) = env.json(&["check", "--fail-on-updates"]);
    assert_eq!(code, 10, "{v}");
    let closed = addon(&v, "Closed");
    assert_eq!(closed["status"], "DOWNLOAD_NOT_ALLOWED");
    assert_eq!(closed["available"]["id"], "6002");
    let msg = closed["message"].as_str().unwrap();
    assert!(
        msg.contains("https://www.curseforge.com/wow/addons/closed/files/6002"),
        "{msg}"
    );
    assert!(msg.contains("wowaddons import"), "{msg}");
    assert_eq!(v["result"]["summary"]["manual_updates"], 1);
    let (_, out, _) = env.run(&["check"]);
    assert!(out.contains("must be downloaded by hand"), "{out}");

    let (code, out, _) = env.run(&["update", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("DOWNLOAD_NOT_ALLOWED"), "{out}");
    assert_eq!(cf.count("/v1/mods/600/files/6002/download-url").await, 0);

    // The import is a normal transaction: it can be rolled back.
    let (code, out, err) = env.run(&["rollback", "--yes"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(snapshot(&env.addons), before);
    assert!(
        env.state("forever")["projects"]
            .get("curseforge:600")
            .is_none()
    );
}

#[test]
fn import_refuses_what_it_cannot_place() {
    let env = Env::new();
    env.setup_forever();
    make_addon(&env.addons, "Closed", "1.0", Some(CLOSED));
    make_addon(&env.addons, "Mystery", "1.0", None);
    let before = snapshot(&env.addons);

    drop_zip(&env, "a.zip", &closed_zip("2.0"));
    drop_zip(&env, "b.zip", &closed_zip("2.1"));
    // Same folder as an installed addon, but no project anywhere.
    let mystery = addon_zip(&[("Mystery", "2.0", None)]);
    drop_zip(&env, "mystery.zip", &mystery);
    // A project that is not installed in this profile.
    drop_zip(&env, "new.zip", &addon_zip(&[("Brand", "1.0", Some(700))]));
    drop_zip(&env, "broken.zip", b"not a zip");

    let (code, v) = env.json(&["--offline", "import", "--yes"]);
    assert_eq!(code, 5, "{v}");
    assert_eq!(skipped_status(&v, "a.zip"), "DUPLICATE");
    assert_eq!(skipped_status(&v, "b.zip"), "DUPLICATE");
    assert_eq!(skipped_status(&v, "mystery.zip"), "UNTRACKED");
    assert_eq!(skipped_status(&v, "new.zip"), "NOT_INSTALLED");
    assert_eq!(skipped_status(&v, "broken.zip"), "ERROR");
    assert_eq!(snapshot(&env.addons), before);
    // Nothing was installed, so every file stays in the inbox.
    assert_eq!(std::fs::read_dir(inbox(&env)).unwrap().count(), 5);

    // A link tells which project an archive without project ID belongs to.
    for f in ["a.zip", "b.zip", "new.zip", "broken.zip"] {
        std::fs::remove_file(inbox(&env).join(f)).unwrap();
    }
    let (code, _, err) = env.run(&["--offline", "link", "Mystery", "--project-id", "800"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = env.run(&["--offline", "import", "--yes"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(env.state("forever")["projects"]["curseforge:800"].is_object());

    // The same file again: nothing to do, and it leaves the inbox.
    drop_zip(&env, "again.zip", &mystery);
    let (code, v) = env.json(&["--offline", "import", "--yes"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(skipped_status(&v, "again.zip"), "ALREADY_INSTALLED");
    assert!(!inbox(&env).join("again.zip").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn blocked_unmanaged_addon_gets_a_link() {
    let cf = FakeCurseForge::start().await;
    let mut env = Env::new();
    env.api_url = Some(cf.uri());
    env.setup_forever();
    make_addon(&env.addons, "Closed", "1.0", Some(CLOSED));
    closed_project(&cf).await;
    let (code, v) = env.json(&["check"]);
    // The file list is not available: still a link to the project's files.
    assert_eq!(code, 0, "{v}");
    let closed = addon(&v, "Closed");
    assert_eq!(closed["status"], "DOWNLOAD_NOT_ALLOWED");
    assert!(
        closed["message"]
            .as_str()
            .unwrap()
            .contains("https://www.curseforge.com/wow/addons/closed/files"),
        "{v}"
    );
}
