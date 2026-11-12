//! `remove`: deleting addons offline, as a transaction that `rollback` can undo.

mod common;

use common::{Env, addon_zip, make_addon, snapshot};

#[test]
fn remove_untracked_and_unmanaged_then_rollback() {
    let env = Env::new();
    env.setup_forever();
    make_addon(&env.addons, "Custom", "1.0", None);
    make_addon(&env.addons, "Details", "9.0", Some(111));
    make_addon(&env.addons, "Details_Options", "9.0", Some(111));
    make_addon(&env.addons, "Keep", "1.0", None);
    let before = snapshot(&env.addons);

    // Dry run: a plan, no change.
    let (code, v) = env.json(&["remove", "custom", "Details_Options", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    let plan = v["result"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 2);
    let details = plan.iter().find(|p| p["addon"] == "Details").unwrap();
    assert_eq!(details["tracking"], "UNMANAGED");
    assert_eq!(
        details["remove"],
        serde_json::json!(["Details", "Details_Options"])
    );
    assert_eq!(snapshot(&env.addons), before);

    // No terminal and no --yes: refused.
    let (code, _, _) = env.run(&["remove", "Custom"]);
    assert_eq!(code, 2);
    assert_eq!(snapshot(&env.addons), before);

    let (code, out, err) = env.run(&["remove", "Custom", "Details", "--yes"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("Removed 2 addons"), "{out}");
    assert!(!env.addons.join("Custom").exists());
    assert!(!env.addons.join("Details").exists());
    assert!(!env.addons.join("Details_Options").exists());
    assert!(env.addons.join("Keep").is_dir());

    let (_, out, _) = env.run(&["history"]);
    assert!(out.contains("remove"), "{out}");

    let (code, out, err) = env.run(&["rollback", "--yes"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(snapshot(&env.addons), before);
}

#[test]
fn remove_managed_addon_updates_state() {
    let env = Env::new();
    env.setup_forever();
    make_addon(&env.addons, "Closed", "1.0", Some(600));
    let inbox = env.data.join("profiles").join("forever").join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(
        inbox.join("Closed-2.0.zip"),
        addon_zip(&[
            ("Closed", "2.0", Some(600)),
            ("Closed_Options", "2.0", None),
        ]),
    )
    .unwrap();
    let (code, _, err) = env.run(&["import", "--yes"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        env.state("forever")["projects"]
            .get("curseforge:600")
            .is_some()
    );
    let before = snapshot(&env.addons);

    // Missing a folder by hand: the rest is removed and the state entry dropped.
    std::fs::remove_dir_all(env.addons.join("Closed_Options")).unwrap();
    let (code, v) = env.json(&["remove", "Closed", "--yes"]);
    assert_eq!(code, 0, "{v}");
    let removed = &v["result"]["removed"][0];
    assert_eq!(removed["tracking"], "MISSING");
    assert_eq!(removed["forget"], serde_json::json!(["Closed_Options"]));
    assert!(!env.addons.join("Closed").exists());
    assert!(
        env.state("forever")["projects"]
            .get("curseforge:600")
            .is_none()
    );
    let (_, out, _) = env.run(&["--offline", "list"]);
    assert!(!out.contains("Closed"), "{out}");

    // A folder put back by hand blocks the rollback instead of colliding with it.
    make_addon(&env.addons, "Closed", "3.0", Some(600));
    let (code, _, err) = env.run(&["rollback", "--yes"]);
    assert_eq!(code, 5, "{err}");
    std::fs::remove_dir_all(env.addons.join("Closed")).unwrap();

    let (code, out, err) = env.run(&["rollback", "--yes"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(
        env.state("forever")["projects"]
            .get("curseforge:600")
            .is_some()
    );
    let mut expected = before;
    expected.retain(|(p, _)| !p.starts_with("Closed_Options"));
    assert_eq!(snapshot(&env.addons), expected);
}

#[test]
fn remove_refuses_protected_and_unknown() {
    let env = Env::new();
    env.setup_forever();
    make_addon(&env.addons, "Blizzard_Test", "1.0", None);
    make_addon(&env.addons, "Custom", "1.0", None);
    let before = snapshot(&env.addons);

    let (code, _, err) = env.run(&["remove", "Custom", "Blizzard_Test", "--yes"]);
    assert_eq!(code, 5, "{err}");
    assert!(err.contains("protected"), "{err}");
    let (code, _, err) = env.run(&["remove", "Nope", "--yes"]);
    assert_eq!(code, 2, "{err}");
    let (code, _, _) = env.run(&["remove", "--yes"]);
    assert_eq!(code, 2);
    assert_eq!(snapshot(&env.addons), before);

    let (code, _, err) = env.run(&["uninstall", "Custom", "--yes"]);
    assert_eq!(code, 0, "{err}");
    assert!(!env.addons.join("Custom").exists());
}
