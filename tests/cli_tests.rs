//! CLI behavior that needs no network: config, profiles, options, exit codes,
//! scan/list, link.

mod common;

use common::{Env, make_addon};

#[test]
fn init_creates_config_once() {
    let env = Env::new();
    let (code, out, _) = env.run(&["init"]);
    assert_eq!(code, 0);
    assert!(out.contains("Created"));
    assert!(env.config.exists());
    let before = std::fs::read_to_string(&env.config).unwrap();
    let (code, out, _) = env.run(&["init"]);
    assert_eq!(code, 0);
    assert!(out.contains("already exists"));
    assert_eq!(std::fs::read_to_string(&env.config).unwrap(), before);
}

#[test]
fn first_run_offline() {
    let env = Env::new();
    env.setup_forever();
    make_addon(&env.addons, "Bartender4", "4.15.0", Some(111));
    make_addon(&env.addons, "CustomAddon", "1.0.0", None);
    std::fs::create_dir_all(env.addons.join("NotAnAddon")).unwrap();

    let (code, out, err) = env.run(&["--offline", "list"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("Bartender4") && out.contains("UNMANAGED"),
        "{out}"
    );
    assert!(
        out.contains("CustomAddon") && out.contains("UNTRACKED"),
        "{out}"
    );
    assert!(!out.contains("NotAnAddon"), "{out}");

    let (code, out, _) = env.run(&["scan"]);
    assert_eq!(code, 0);
    assert!(out.contains("NotAnAddon: no matching .toc file"), "{out}");
    assert!(out.contains("project: curseforge:111"), "{out}");
}

#[test]
fn first_profile_becomes_default() {
    let env = Env::new();
    env.setup_forever();
    let (code, v) = env.json(&["profile", "list"]);
    assert_eq!(code, 0);
    assert_eq!(v["result"]["default_profile"], "forever");
}

#[test]
fn profile_validation() {
    let env = Env::new();
    env.run(&["init"]);
    let addons = env.addons.to_string_lossy().to_string();
    let (code, _, err) = env.run(&[
        "profile",
        "add",
        "Bad Name",
        "--client",
        "forever",
        "--addons-path",
        &addons,
    ]);
    assert_eq!(code, 2, "{err}");
    let (code, _, _) = env.run(&[
        "profile",
        "add",
        "x",
        "--client",
        "wotlk",
        "--addons-path",
        &addons,
    ]);
    assert_eq!(code, 2);
    let missing = env.dir.path().join("nope").to_string_lossy().to_string();
    let (code, _, _) = env.run(&[
        "profile",
        "add",
        "x",
        "--client",
        "forever",
        "--addons-path",
        &missing,
    ]);
    assert_eq!(code, 1);

    env.run(&[
        "profile",
        "add",
        "forever",
        "--client",
        "forever",
        "--addons-path",
        &addons,
    ]);
    // Same folder again: refused.
    let (code, _, err) = env.run(&[
        "profile",
        "add",
        "other",
        "--client",
        "retail",
        "--addons-path",
        &addons,
    ]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("already uses"), "{err}");
    // Custom client needs a version type.
    let other = env.dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let other = other.to_string_lossy().to_string();
    let (code, _, _) = env.run(&[
        "profile",
        "add",
        "c",
        "--client",
        "custom",
        "--addons-path",
        &other,
    ]);
    assert_eq!(code, 2);
}

#[test]
fn profile_set_unset_show_remove() {
    let env = Env::new();
    env.setup_forever();
    assert_eq!(
        env.run(&["profile", "set", "forever", "game_version", "1.60.1"])
            .0,
        0
    );
    assert_eq!(
        env.run(&["profile", "set", "forever", "channel", "nightly"])
            .0,
        2
    );
    assert_eq!(env.run(&["profile", "set", "forever", "nope", "1"]).0, 2);
    let (_, v) = env.json(&["profile", "show", "forever"]);
    assert_eq!(v["result"]["keys"]["game_version"], "1.60.1");
    assert_eq!(
        env.run(&["profile", "unset", "forever", "game_version"]).0,
        0
    );
    assert_eq!(env.run(&["profile", "unset", "forever", "client"]).0, 2);
    let (_, v) = env.json(&["profile", "show", "forever"]);
    assert!(v["result"]["keys"]["game_version"].is_null());

    // Removing needs confirmation; without a TTY it is a usage error.
    assert_eq!(env.run(&["profile", "remove", "forever"]).0, 2);
    assert_eq!(env.run(&["profile", "remove", "forever", "--yes"]).0, 0);
    let (_, v) = env.json(&["profile", "list"]);
    assert!(v["result"]["default_profile"].is_null());
    // AddOns folder untouched.
    assert!(env.addons.exists());
}

#[test]
fn config_set_keeps_comments_and_validates() {
    let env = Env::new();
    env.run(&["init"]);
    let mut text = std::fs::read_to_string(&env.config).unwrap();
    text.push_str("# my own comment\n");
    std::fs::write(&env.config, text).unwrap();

    assert_eq!(env.run(&["config", "set", "defaults.channel", "beta"]).0, 0);
    assert_eq!(
        env.run(&["config", "set", "defaults.backup_retention", "10"])
            .0,
        0
    );
    assert_eq!(
        env.run(&["config", "set", "defaults.backup_retention", "0"])
            .0,
        2
    );
    assert_eq!(env.run(&["config", "set", "defaults.chanel", "beta"]).0, 2);
    assert_eq!(env.run(&["config", "set", "default_profile", "ghost"]).0, 2);
    let text = std::fs::read_to_string(&env.config).unwrap();
    assert!(text.contains("# my own comment"), "{text}");
    assert!(text.contains("channel = \"beta\""), "{text}");

    let (_, out, _) = env.run(&["config", "get", "defaults.channel"]);
    assert_eq!(out.trim(), "beta");
    assert_eq!(env.run(&["config", "unset", "defaults.channel"]).0, 0);
    let (_, v) = env.json(&["config", "get", "defaults.channel"]);
    assert_eq!(v["result"]["value"], "release");
    assert_eq!(v["result"]["source"], "default");
}

#[test]
fn invalid_config_file_is_reported() {
    let env = Env::new();
    std::fs::create_dir_all(env.config.parent().unwrap()).unwrap();
    std::fs::write(
        &env.config,
        "config_version = 1\n[defaults]\nchanel = \"beta\"\n",
    )
    .unwrap();
    let (code, _, err) = env.run(&["config", "list"]);
    assert_eq!(code, 1);
    assert!(err.contains("chanel"), "{err}");
}

#[test]
fn options_only_where_they_belong() {
    let env = Env::new();
    env.setup_forever();
    // Wrong options for the command: usage error (exit 2).
    assert_eq!(env.run(&["scan", "--dry-run"]).0, 2);
    assert_eq!(env.run(&["list", "--channel", "beta"]).0, 2);
    assert_eq!(env.run(&["check", "--addons-path", "x"]).0, 2);
    assert_eq!(env.run(&["--profile", "forever", "config", "list"]).0, 2);
    assert_eq!(env.run(&["-v", "-q", "list"]).0, 2);
    // --file-id needs exactly one --addon.
    assert_eq!(env.run(&["update", "--file-id", "1"]).0, 2);
}

#[test]
fn network_commands_need_a_key_or_network() {
    let mut env = Env::new();
    env.setup_forever();
    let (code, _, err) = env.run(&["--offline", "check"]);
    assert_eq!(code, 1, "{err}");
    env.api_key = false;
    let (code, _, err) = env.run(&["check"]);
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("API key is missing"), "{err}");
    let (code, v) = env.json(&["check"]);
    assert_eq!(code, 3);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["kind"], "missing_api_key");
}

#[test]
fn no_profile_error() {
    let env = Env::new();
    env.run(&["init"]);
    let (code, _, err) = env.run(&["list"]);
    assert_eq!(code, 1);
    assert!(err.contains("no profile selected"), "{err}");
    let (code, _, _) = env.run(&["--profile", "ghost", "list"]);
    assert_eq!(code, 1);
}

#[test]
fn list_any_folder_read_only() {
    let env = Env::new();
    make_addon(&env.addons, "Details", "2025.11.1", Some(61284));
    let addons = env.addons.to_string_lossy().to_string();
    let (code, v) = env.json(&["list", "--addons-path", &addons]);
    assert_eq!(code, 0);
    assert_eq!(v["result"]["addons"][0]["name"], "Details");
    assert_eq!(v["result"]["addons"][0]["tracking"], "UNMANAGED");
}

#[test]
fn link_and_unlink_offline() {
    let env = Env::new();
    env.setup_forever();
    make_addon(&env.addons, "CustomAddon", "1.0.0", None);
    let (code, _, err) = env.run(&["--offline", "link", "customaddon", "--project-id", "222"]);
    assert_eq!(code, 0, "{err}");
    let (_, v) = env.json(&["list"]);
    let a = &v["result"]["addons"][0];
    assert_eq!(a["project"], "curseforge:222");
    assert_eq!(a["id_source"], "link");
    assert_eq!(
        env.run(&["--offline", "link", "Ghost", "--project-id", "1"])
            .0,
        2
    );
    assert_eq!(
        env.run(&["--offline", "link", "CustomAddon", "--project-id", "abc"])
            .0,
        2
    );
    assert_eq!(env.run(&["unlink", "CustomAddon"]).0, 0);
    assert_eq!(env.run(&["unlink", "CustomAddon"]).0, 1);
    let (_, v) = env.json(&["list"]);
    assert_eq!(v["result"]["addons"][0]["tracking"], "UNTRACKED");
}

#[test]
fn link_overrides_toc() {
    let env = Env::new();
    env.setup_forever();
    make_addon(&env.addons, "Thing", "1.0", Some(5));
    env.run(&["--offline", "link", "Thing", "--project-id", "6"]);
    let (_, v) = env.json(&["list"]);
    assert_eq!(v["result"]["addons"][0]["project"], "curseforge:6");
}

#[test]
fn conflicting_toc_ids() {
    let env = Env::new();
    env.setup_forever();
    make_addon(&env.addons, "Multi", "1.0", Some(5));
    std::fs::write(
        env.addons.join("Multi").join("Multi_Camelot.toc"),
        common::toc("Multi", "1.1", Some(6)),
    )
    .unwrap();
    let (_, v) = env.json(&["list"]);
    let a = &v["result"]["addons"][0];
    assert_eq!(a["tracking"], "CONFLICT");
    // The Forever TOC is the one read for the version.
    assert_eq!(a["local_version"], "1.1");
}

#[cfg(unix)]
#[test]
fn symlinked_addon_is_protected() {
    let env = Env::new();
    env.setup_forever();
    let dev = env.dir.path().join("dev").join("MyAddon");
    std::fs::create_dir_all(&dev).unwrap();
    std::fs::write(
        dev.join("MyAddon.toc"),
        common::toc("MyAddon", "dev", Some(9)),
    )
    .unwrap();
    std::os::unix::fs::symlink(&dev, env.addons.join("MyAddon")).unwrap();
    let (_, v) = env.json(&["list"]);
    assert_eq!(v["result"]["addons"][0]["tracking"], "PROTECTED");
}

#[test]
fn history_empty_and_rollback_nothing() {
    let env = Env::new();
    env.setup_forever();
    let (code, out, _) = env.run(&["history"]);
    assert_eq!(code, 0);
    assert!(out.contains("No transactions"));
    assert_eq!(env.run(&["rollback", "--yes"]).0, 1);
}

#[test]
fn doctor_offline() {
    let env = Env::new();
    env.setup_forever();
    let (code, v) = env.json(&["--offline", "doctor"]);
    assert_eq!(code, 0, "{v}");
    let checks = v["result"]["checks"].as_array().unwrap();
    assert!(
        checks
            .iter()
            .any(|c| c["name"] == "profile forever" && c["level"] == "ok")
    );
    assert!(
        checks
            .iter()
            .any(|c| c["name"] == "profile forever work dir" && c["level"] == "ok")
    );
}

#[test]
fn api_key_never_printed() {
    let env = Env::new();
    env.setup_forever();
    env.run(&["config", "set", "defaults.max_retries", "0"]);
    for args in [&["check"][..], &["doctor"][..], &["-vvv", "check"][..]] {
        let (_, out, err) = env.run(args);
        assert!(!out.contains("test-key-123") && !err.contains("test-key-123"));
    }
}
