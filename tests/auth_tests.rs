//! `wowaddons auth …`. The system secret store is disabled in tests
//! (`WOWADDONS_NO_KEYRING=1`); the store logic itself is unit-tested in `secrets.rs`.

mod common;

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::Env;

#[test]
fn status_reports_the_source_never_the_value() {
    let mut env = Env::new();
    let (code, v) = env.json(&["auth", "status"]);
    assert_eq!(code, 0);
    assert_eq!(v["result"]["source"], "environment");
    assert!(!v.to_string().contains("test-key-123"));

    env.api_key = false;
    let (code, out, _) = env.run(&["auth", "status"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("not set") && out.contains("auth set-key"),
        "{out}"
    );
}

#[test]
fn missing_key_hint_points_to_auth() {
    let mut env = Env::new();
    env.setup_forever();
    env.api_key = false;
    let (code, _, err) = env.run(&["check"]);
    assert_eq!(code, 3);
    assert!(err.contains("wowaddons auth set-key"), "{err}");
}

#[test]
fn set_key_needs_a_terminal_or_stdin() {
    let env = Env::new();
    // No terminal and no --stdin: usage error, nothing read.
    let (code, _, err) = env.run_with_stdin(&["auth", "set-key"], "abc\n");
    assert_eq!(code, 2, "{err}");
    // Empty key.
    let (code, _, _) = env.run_with_stdin(&["auth", "set-key", "--stdin", "--offline"], "\n");
    assert_eq!(code, 2);
    // The key is never accepted as an argument.
    assert_eq!(env.run(&["auth", "set-key", "abc"]).0, 2);
}

#[test]
fn set_key_reports_a_disabled_store() {
    let env = Env::new();
    let (code, _, err) =
        env.run_with_stdin(&["auth", "set-key", "--stdin", "--offline"], "my-key\n");
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("disabled"), "{err}");
    assert!(!err.contains("my-key"));
    // Clearing a disabled store is harmless.
    let (code, out, _) = env.run(&["auth", "clear"]);
    assert_eq!(code, 0);
    assert!(out.contains("No API key was stored"));
}

#[tokio::test(flavor = "multi_thread")]
async fn set_key_refuses_a_key_rejected_by_curseforge() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/games/1/version-types"))
        .and(header("x-api-key", "bad-key"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let mut env = Env::new();
    env.api_url = Some(server.uri());
    let (code, _, err) = env.run_with_stdin(&["auth", "set-key", "--stdin"], "bad-key\n");
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("not saved"), "{err}");
    assert!(!err.contains("bad-key"));
}

#[tokio::test(flavor = "multi_thread")]
async fn set_key_checks_a_good_key_before_saving() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/games/1/version-types"))
        .and(header("x-api-key", "good-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": [] })))
        .expect(1)
        .mount(&server)
        .await;
    let mut env = Env::new();
    env.api_url = Some(server.uri());
    // The key is accepted by the API; saving then fails only because the store is disabled.
    let (code, _, err) = env.run_with_stdin(&["auth", "set-key", "--stdin"], "good-key\n");
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("disabled"), "{err}");
}
