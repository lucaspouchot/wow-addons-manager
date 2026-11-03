//! Shared helpers for integration tests: isolated config/data dirs, addon
//! fixtures, ZIP builders and a fake CurseForge API.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use tempfile::TempDir;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const FOREVER_TYPE: u32 = 88568;
pub const RETAIL_TYPE: u32 = 517;

/// One isolated environment: config, data dir and an AddOns folder.
pub struct Env {
    pub dir: TempDir,
    pub config: PathBuf,
    pub data: PathBuf,
    pub addons: PathBuf,
    pub api_url: Option<String>,
    pub api_key: bool,
}

impl Env {
    pub fn new() -> Env {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("config").join("config.toml");
        let data = dir.path().join("data");
        let addons = dir
            .path()
            .join("game")
            .join("_forever_")
            .join("Interface")
            .join("AddOns");
        std::fs::create_dir_all(&addons).expect("addons dir");
        Env {
            dir,
            config,
            data,
            addons,
            api_url: None,
            api_key: true,
        }
    }

    pub fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_wowaddons"));
        c.env("WOWADDONS_CONFIG", &self.config)
            .env("WOWADDONS_DATA_DIR", &self.data)
            .env_remove("RUST_LOG")
            // Never read or write the real system secret store from tests.
            .env("WOWADDONS_NO_KEYRING", "1")
            .env("NO_COLOR", "1");
        match &self.api_url {
            Some(u) => c.env("WOWADDONS_CURSEFORGE_API_URL", u),
            // An unroutable URL: tests must never reach the real API.
            None => c.env("WOWADDONS_CURSEFORGE_API_URL", "http://127.0.0.1:9"),
        };
        if self.api_key {
            c.env("WOWADDONS_CURSEFORGE_API_KEY", "test-key-123");
        } else {
            c.env_remove("WOWADDONS_CURSEFORGE_API_KEY");
        }
        c
    }

    /// Run and return (exit code, stdout, stderr).
    pub fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd().args(args).output().expect("run wowaddons");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    /// Run with `input` on stdin.
    pub fn run_with_stdin(&self, args: &[&str], input: &str) -> (i32, String, String) {
        use std::io::Write as _;
        use std::process::Stdio;
        let mut child = self
            .cmd()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wowaddons");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
        let out = child.wait_with_output().expect("wait");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    /// Run with `--json` and parse stdout.
    pub fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let (code, out, err) = self.run(&all);
        let v: Value = serde_json::from_str(&out)
            .unwrap_or_else(|e| panic!("invalid JSON ({e}): stdout={out}\nstderr={err}"));
        (code, v)
    }

    /// `init` + a `forever` profile on `self.addons`.
    pub fn setup_forever(&self) {
        let (c, _, e) = self.run(&["init"]);
        assert_eq!(c, 0, "{e}");
        let addons = self.addons.to_string_lossy().to_string();
        let (c, _, e) = self.run(&[
            "profile",
            "add",
            "forever",
            "--client",
            "forever",
            "--addons-path",
            &addons,
        ]);
        assert_eq!(c, 0, "{e}");
    }

    pub fn state(&self, profile: &str) -> Value {
        let p = self.data.join("profiles").join(profile).join("state.json");
        serde_json::from_str(&std::fs::read_to_string(p).expect("state")).expect("state json")
    }
}

/// TOC text for a fictional addon.
pub fn toc(title: &str, version: &str, project: Option<u64>) -> String {
    let mut s = format!("## Interface: 16001\n## Title: {title}\n## Version: {version}\n");
    if let Some(p) = project {
        s.push_str(&format!("## X-Curse-Project-ID: {p}\n"));
    }
    s.push_str(&format!("{title}.lua\n"));
    s
}

/// Create `AddOns/<folder>/<folder>.toc` and a Lua file.
pub fn make_addon(addons: &Path, folder: &str, version: &str, project: Option<u64>) {
    let dir = addons.join(folder);
    std::fs::create_dir_all(&dir).expect("addon dir");
    std::fs::write(
        dir.join(format!("{folder}.toc")),
        toc(folder, version, project),
    )
    .expect("toc");
    std::fs::write(
        dir.join(format!("{folder}.lua")),
        format!("-- {folder} {version}\n"),
    )
    .expect("lua");
}

/// Snapshot of a folder tree: (relative path, content) sorted.
pub fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(root)
                .expect("prefix")
                .to_string_lossy()
                .replace('\\', "/");
            if p.is_dir() {
                out.push((format!("{rel}/"), Vec::new()));
                walk(root, &p, out);
            } else {
                out.push((rel, std::fs::read(&p).unwrap_or_default()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Build a ZIP. Names ending with `/` are directories.
pub fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            if name.ends_with('/') {
                w.add_directory(*name, opts).expect("dir");
            } else {
                w.start_file(*name, opts).expect("file");
                w.write_all(data).expect("write");
            }
        }
        w.finish().expect("finish");
    }
    buf.into_inner()
}

/// A valid addon ZIP with one folder per entry: (folder, version, project id).
pub fn addon_zip(folders: &[(&str, &str, Option<u64>)]) -> Vec<u8> {
    let tocs: Vec<(String, String)> = folders
        .iter()
        .map(|(f, v, p)| (format!("{f}/{f}.toc"), toc(f, v, *p)))
        .collect();
    let luas: Vec<(String, String)> = folders
        .iter()
        .map(|(f, v, _)| (format!("{f}/{f}.lua"), format!("-- {f} {v}\n")))
        .collect();
    let mut entries: Vec<(&str, &[u8])> = Vec::new();
    for (n, c) in tocs.iter().chain(luas.iter()) {
        entries.push((n.as_str(), c.as_bytes()));
    }
    zip_bytes(&entries)
}

pub fn sha1_hex(bytes: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// A CurseForge file JSON object.
pub fn cf_file(
    id: u64,
    mod_id: u64,
    display: &str,
    date: &str,
    types: &[u32],
    bytes: Option<&[u8]>,
) -> Value {
    let hashes = match bytes {
        Some(b) => json!([{ "value": sha1_hex(b), "algo": 1 }]),
        None => json!([]),
    };
    json!({
        "id": id,
        "gameId": 1,
        "modId": mod_id,
        "isAvailable": true,
        "displayName": display,
        "fileName": format!("{display}.zip"),
        "releaseType": 1,
        "fileStatus": 4,
        "hashes": hashes,
        "fileDate": date,
        "fileLength": bytes.map(|b| b.len()).unwrap_or(0),
        "gameVersions": ["1.60.1"],
        "sortableGameVersions": types.iter().map(|t| json!({
            "gameVersionName": "1.60.1", "gameVersion": "1.60.1", "gameVersionTypeId": t
        })).collect::<Vec<_>>(),
    })
}

/// Fake CurseForge API.
pub struct FakeCurseForge {
    pub server: MockServer,
}

impl FakeCurseForge {
    pub async fn start() -> FakeCurseForge {
        let server = MockServer::start().await;
        let f = FakeCurseForge { server };
        f.mount_base().await;
        f
    }

    pub fn uri(&self) -> String {
        self.server.uri()
    }

    pub async fn mount_base(&self) {
        Mock::given(method("GET"))
            .and(path("/v1/games/1/version-types"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [
                { "id": RETAIL_TYPE, "gameId": 1, "name": "WoW Retail", "slug": "wow_retail" },
                { "id": 67408, "gameId": 1, "name": "WoW Classic", "slug": "wow-classic" },
                { "id": FOREVER_TYPE, "gameId": 1, "name": "WoW Forever", "slug": "wow-forever" },
            ]})))
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/games/1/versions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [
                { "type": FOREVER_TYPE, "versions": ["1.60.0", "1.60.1"] },
                { "type": RETAIL_TYPE, "versions": ["11.2.5"] },
            ]})))
            .mount(&self.server)
            .await;
    }

    /// `POST /v1/mods` returns these projects: (id, name, game id, allow distribution).
    pub async fn projects(&self, mods: &[(u64, &str, u32, bool)]) {
        let data: Vec<Value> = mods
            .iter()
            .map(|(id, name, game, allow)| json!({
                "id": id, "gameId": game, "name": name, "allowModDistribution": allow, "isAvailable": true,
            }))
            .collect();
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": data })))
            .mount(&self.server)
            .await;
    }

    /// The file list of a project (one page).
    pub async fn files(&self, mod_id: u64, files: Vec<Value>) {
        let n = files.len();
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/{mod_id}/files")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": files,
                "pagination": { "index": 0, "pageSize": 50, "resultCount": n, "totalCount": n },
            })))
            .mount(&self.server)
            .await;
    }

    /// Single file lookup, download URL and archive bytes.
    pub async fn downloadable(&self, file: &Value, bytes: Vec<u8>) {
        let mod_id = file["modId"].as_u64().expect("modId");
        let id = file["id"].as_u64().expect("id");
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/{mod_id}/files/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": file })))
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/{mod_id}/files/{id}/download-url")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": format!("{}/cdn/{id}.zip", self.server.uri())
            })))
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/cdn/{id}.zip")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
            .mount(&self.server)
            .await;
    }

    pub async fn reset(&self) {
        self.server.reset().await;
        self.mount_base().await;
    }

    /// Requests received whose path matches a regex.
    pub async fn count(&self, re: &str) -> usize {
        let re = regex_lite(re);
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| re(r.url.path()))
            .count()
    }
}

/// Minimal "starts with" matcher to avoid a regex dependency.
fn regex_lite(prefix: &str) -> impl Fn(&str) -> bool + '_ {
    move |p: &str| p.starts_with(prefix)
}

/// Mount a mock that matches any path with a regex (re-exported for tests).
pub fn any_path(re: &str) -> wiremock::MockBuilder {
    Mock::given(path_regex(re))
}
