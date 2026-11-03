//! CurseForge provider against a fake HTTP server.

mod common;

use std::time::Duration;

use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{FOREVER_TYPE, cf_file};
use wow_addons_manager::domain::ProjectRef;
use wow_addons_manager::net::{ApiKey, NetConfig};
use wow_addons_manager::providers::curseforge::CurseForgeProvider;
use wow_addons_manager::providers::{AddonProvider, FileQuery, ProviderError};

fn provider(server: &MockServer) -> CurseForgeProvider {
    let cfg = NetConfig {
        timeout: Duration::from_millis(500),
        max_retries: 2,
        retry_base: Duration::from_millis(5),
    };
    CurseForgeProvider::with_base_url(ApiKey::new("k-secret").unwrap(), cfg, &server.uri()).unwrap()
}

fn p(id: &str) -> ProjectRef {
    ProjectRef::curseforge(id)
}

#[tokio::test]
async fn sends_key_and_reads_version_types() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/games/1/version-types"))
        .and(header("x-api-key", "k-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [
            { "id": FOREVER_TYPE, "name": "WoW Forever", "slug": "wow-forever" }
        ]})))
        .mount(&server)
        .await;
    let types = provider(&server).version_types().await.unwrap();
    assert_eq!(types[0].id, FOREVER_TYPE);
}

#[tokio::test]
async fn paginates_file_lists() {
    let server = MockServer::start().await;
    let page = |range: std::ops::Range<u64>, index: u32| {
        let files: Vec<_> = range
            .map(|i| {
                cf_file(
                    1000 + i,
                    7,
                    &format!("v{i}"),
                    "2026-10-01T00:00:00Z",
                    &[FOREVER_TYPE],
                    None,
                )
            })
            .collect();
        json!({ "data": files, "pagination": { "index": index, "pageSize": 50, "resultCount": 50, "totalCount": 120 } })
    };
    for (index, range) in [(0, 0..50), (50, 50..100), (100, 100..120)] {
        Mock::given(method("GET"))
            .and(path("/v1/mods/7/files"))
            .and(query_param("index", index.to_string()))
            .and(query_param("gameVersionTypeId", FOREVER_TYPE.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(range, index)))
            .mount(&server)
            .await;
    }
    let q = FileQuery {
        game_version_type_id: Some(FOREVER_TYPE),
        game_version: None,
    };
    let files = provider(&server).list_files(&p("7"), &q).await.unwrap();
    assert_eq!(files.len(), 120);
    assert_eq!(files[0].game_version_type_ids, vec![FOREVER_TYPE]);
}

#[tokio::test]
async fn empty_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/mods/7/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [], "pagination": { "index": 0, "pageSize": 50, "resultCount": 0, "totalCount": 0 }
        })))
        .mount(&server)
        .await;
    let files = provider(&server)
        .list_files(&p("7"), &FileQuery::default())
        .await
        .unwrap();
    assert!(files.is_empty());
}

async fn status_error(status: u16) -> ProviderError {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/games/1/version-types"))
        .respond_with(ResponseTemplate::new(status))
        .mount(&server)
        .await;
    provider(&server).version_types().await.unwrap_err()
}

#[tokio::test]
async fn http_errors() {
    assert!(matches!(
        status_error(401).await,
        ProviderError::Auth { status: 401, .. }
    ));
    assert!(matches!(
        status_error(403).await,
        ProviderError::Auth { status: 403, .. }
    ));
    assert!(status_error(401).await.is_fatal());
    assert!(matches!(
        status_error(404).await,
        ProviderError::NotFound(_)
    ));
    assert!(matches!(
        status_error(429).await,
        ProviderError::RateLimited
    ));
    assert!(matches!(
        status_error(500).await,
        ProviderError::Http { status: 500, .. }
    ));
}

#[tokio::test]
async fn no_retry_on_auth_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let _ = provider(&server).version_types().await;
}

#[tokio::test]
async fn retries_transient_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/games/1/version-types"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/games/1/version-types"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/games/1/version-types"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
        .mount(&server)
        .await;
    assert!(provider(&server).version_types().await.unwrap().is_empty());
}

#[tokio::test]
async fn timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let err = provider(&server).version_types().await.unwrap_err();
    assert!(matches!(err, ProviderError::Timeout), "{err:?}");
}

#[tokio::test]
async fn missing_download_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/mods/7/files/1001/download-url"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": null })))
        .mount(&server)
        .await;
    let file = cf_file(1001, 7, "v1", "2026-10-01T00:00:00Z", &[FOREVER_TYPE], None);
    let file = to_domain(&server, file).await;
    let dir = tempfile::tempdir().unwrap();
    let err = provider(&server)
        .download(&file, &dir.path().join("a.zip"), 1 << 20, &|_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::DownloadNotAllowed));
}

#[tokio::test]
async fn refuses_redirect_to_unknown_host_and_never_sends_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/mods/7/files/1001/download-url"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "data": format!("{}/cdn/a.zip", server.uri()) })),
        )
        .mount(&server)
        .await;
    // The CDN redirects to another host: refused.
    Mock::given(method("GET"))
        .and(path("/cdn/a.zip"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("Location", "https://evil.example.com/a.zip"),
        )
        .mount(&server)
        .await;
    let file = to_domain(
        &server,
        cf_file(1001, 7, "v1", "2026-10-01T00:00:00Z", &[FOREVER_TYPE], None),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let err = provider(&server)
        .download(&file, &dir.path().join("a.zip"), 1 << 20, &|_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::HostNotAllowed(_)), "{err:?}");
}

#[tokio::test]
async fn refuses_download_url_on_unknown_host() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/mods/7/files/1001/download-url"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "data": "https://evil.example.com/a.zip" })),
        )
        .mount(&server)
        .await;
    let file = to_domain(
        &server,
        cf_file(1001, 7, "v1", "2026-10-01T00:00:00Z", &[FOREVER_TYPE], None),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let err = provider(&server)
        .download(&file, &dir.path().join("a.zip"), 1 << 20, &|_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::HostNotAllowed(_)), "{err:?}");
}

#[tokio::test]
async fn download_size_limit_and_hash() {
    let server = MockServer::start().await;
    let bytes = vec![7u8; 4096];
    let file = cf_file(
        1001,
        7,
        "v1",
        "2026-10-01T00:00:00Z",
        &[FOREVER_TYPE],
        Some(&bytes),
    );
    common_downloadable(&server, &file, bytes.clone()).await;
    let file = to_domain(&server, file).await;
    let dir = tempfile::tempdir().unwrap();
    let pr = provider(&server);
    let got = pr
        .download(&file, &dir.path().join("a.zip"), 1 << 20, &|_| {})
        .await
        .unwrap();
    assert_eq!(got.size, 4096);
    assert_eq!(Some(got.sha1.clone()), file.sha1);
    let err = pr
        .download(&file, &dir.path().join("b.zip"), 1000, &|_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::TooLarge { .. }));
}

#[tokio::test]
async fn projects_batch_and_other_game() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/mods"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [
            { "id": 1, "gameId": 1, "name": "A", "allowModDistribution": true },
            { "id": 2, "gameId": 432, "name": "Minecraft mod", "allowModDistribution": true },
        ]})))
        .mount(&server)
        .await;
    let got = provider(&server)
        .get_projects(&[p("1"), p("2"), p("3")])
        .await
        .unwrap();
    assert_eq!(got.len(), 2);
    assert!(got.iter().find(|x| x.project == p("1")).unwrap().is_wow);
    assert!(!got.iter().find(|x| x.project == p("2")).unwrap().is_wow);
}

#[test]
fn base_url_override_is_loopback_only() {
    let cfg = NetConfig::default();
    assert!(
        CurseForgeProvider::with_base_url(
            ApiKey::new("k").unwrap(),
            cfg.clone(),
            "https://evil.example.com"
        )
        .is_err()
    );
    assert!(
        CurseForgeProvider::with_base_url(ApiKey::new("k").unwrap(), cfg, "http://127.0.0.1:1")
            .is_ok()
    );
}

// --- helpers ---

async fn common_downloadable(server: &MockServer, file: &serde_json::Value, bytes: Vec<u8>) {
    let id = file["id"].as_u64().unwrap();
    let mod_id = file["modId"].as_u64().unwrap();
    Mock::given(method("GET"))
        .and(path(format!("/v1/mods/{mod_id}/files/{id}/download-url")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "data": format!("{}/cdn/{id}.zip", server.uri()) })),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/cdn/{id}.zip")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
        .mount(server)
        .await;
}

/// Turn a JSON file into the domain type through the provider itself.
async fn to_domain(
    server: &MockServer,
    file: serde_json::Value,
) -> wow_addons_manager::domain::RemoteFile {
    let id = file["id"].as_u64().unwrap();
    let mod_id = file["modId"].as_u64().unwrap();
    Mock::given(method("GET"))
        .and(path(format!("/v1/mods/{mod_id}/files/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": file })))
        .mount(server)
        .await;
    provider(server)
        .get_file(&p(&mod_id.to_string()), &id.to_string())
        .await
        .unwrap()
}
