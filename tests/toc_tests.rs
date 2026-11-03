//! Scanner on real folders: TOC choice per client and grouping.

mod common;

use common::{make_addon, toc};
use wow_addons_manager::domain::{ProjectRef, Tracking, WowClient};
use wow_addons_manager::scanner::{build_entries, scan};
use wow_addons_manager::storage::State;

fn suffixes(c: WowClient) -> Vec<String> {
    c.info()
        .toc_suffixes
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn picks_the_toc_of_the_active_client() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path();
    make_addon(a, "Example", "base", Some(1));
    std::fs::write(
        a.join("Example/Example_Mainline.toc"),
        toc("Example", "retail", Some(1)),
    )
    .unwrap();
    std::fs::write(
        a.join("Example/example_camelot.TOC"),
        toc("Example", "forever", Some(1)),
    )
    .unwrap();
    std::fs::write(
        a.join("Example/Example_Vanilla.toc"),
        toc("Example", "era", Some(1)),
    )
    .unwrap();

    let v = |c| {
        scan(a, &suffixes(c)).unwrap().addons[0]
            .toc_version
            .clone()
            .unwrap()
    };
    assert_eq!(v(WowClient::Forever), "forever");
    assert_eq!(v(WowClient::Retail), "retail");
    assert_eq!(v(WowClient::ClassicEra), "era");
    assert_eq!(v(WowClient::Classic), "base");
}

#[test]
fn only_client_specific_toc() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path();
    std::fs::create_dir_all(a.join("OnlyRetail")).unwrap();
    std::fs::write(
        a.join("OnlyRetail/OnlyRetail_Mainline.toc"),
        toc("OnlyRetail", "1", None),
    )
    .unwrap();
    let inv = scan(a, &suffixes(WowClient::Forever)).unwrap();
    assert_eq!(inv.addons.len(), 1);
    assert!(
        inv.addons[0]
            .warnings
            .iter()
            .any(|w| w.contains("no TOC file for this client"))
    );
}

#[test]
fn groups_folders_by_project_and_state() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path();
    make_addon(a, "Suite", "1", Some(10));
    make_addon(a, "Suite_Options", "1", Some(10));
    make_addon(a, "Managed", "2", None);
    make_addon(a, "Managed_Config", "2", None);
    make_addon(a, "Loose", "3", None);
    std::fs::create_dir_all(a.join("Blizzard_Junk")).unwrap();

    let mut state = State::new("p");
    state.projects.insert(
        ProjectRef::curseforge("20"),
        serde_json::from_value(serde_json::json!({
            "file_id": "5", "display_version": "2", "file_date": "2026-01-01T00:00:00Z",
            "folders": ["Managed", "Managed_Config", "Managed_Gone"], "client": "forever",
            "game_version_type_id": 88568, "installed_at": "2026-01-02T00:00:00Z", "transaction_id": "t"
        }))
        .unwrap(),
    );
    let inv = scan(a, &[]).unwrap();
    let entries = build_entries(&inv, &state);
    let find = |n: &str| entries.iter().find(|e| e.name == n).unwrap();
    assert_eq!(find("Suite").folders, vec!["Suite", "Suite_Options"]);
    assert_eq!(find("Suite").tracking, Tracking::Unmanaged);
    assert_eq!(find("Managed").tracking, Tracking::Missing);
    assert_eq!(find("Managed").missing_folders, vec!["Managed_Gone"]);
    assert_eq!(find("Loose").tracking, Tracking::Untracked);
    assert_eq!(find("Blizzard_Junk").tracking, Tracking::Protected);
    assert_eq!(entries.len(), 4);
}
