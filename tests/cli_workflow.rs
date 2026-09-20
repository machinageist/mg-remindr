// Author: Jeff
// Date: 2026-09-20
// Description: The binary itself, against a store of its own — migration, projects and tags
// Notes: $MG_REMINDR_DB points the run at a throwaway file, so a test never reaches the store
//        Jeff's reminders live in

use std::process::Command;
use tempfile::TempDir;

// Run one command against the test store and read its JSON
fn run_cli(store: &TempDir, arguments: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_mg-remindr"))
        .args(arguments)
        .env("MG_REMINDR_DB", store.path().join("remindr.sqlite"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn cli_executes_migration_project_and_tag_workflows() {
    let store = tempfile::tempdir().expect("temporary directory");

    let pending = run_cli(&store, &["migration", "status"]);
    assert!(
        pending
            .as_array()
            .unwrap()
            .iter()
            .all(|item| !item["applied"].as_bool().unwrap())
    );
    let applied = run_cli(&store, &["migration", "apply"]);
    assert!(
        applied
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["applied"].as_bool().unwrap())
    );

    let project_id = "00000000-0000-0000-0000-000000000111";
    let project_v1 = format!(
        r#"{{"id":"{project_id}","name":"CLI project","lifecycle":"open","version":1,"created_at":"2026-08-29T12:00:00Z","updated_at":"2026-08-29T12:00:00Z"}}"#
    );
    let project_v2 = format!(
        r#"{{"id":"{project_id}","name":"CLI project renamed","lifecycle":"completed","version":2,"created_at":"2026-08-29T12:00:00Z","updated_at":"2026-08-29T12:00:01Z"}}"#
    );
    assert_eq!(
        run_cli(&store, &["project", "create", "--json", &project_v1])["version"],
        1
    );
    assert_eq!(
        run_cli(&store, &["project", "find", project_id])["name"],
        "CLI project"
    );
    assert_eq!(
        run_cli(&store, &["project", "list"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        run_cli(
            &store,
            &[
                "project",
                "replace",
                "--expected-version",
                "1",
                "--json",
                &project_v2
            ]
        )["version"],
        2
    );

    let tag_id = "00000000-0000-0000-0000-000000000222";
    let tag_v1 = format!(
        r#"{{"id":"{tag_id}","name":"CLI tag","version":1,"created_at":"2026-08-29T12:00:00Z","updated_at":"2026-08-29T12:00:00Z"}}"#
    );
    let tag_v2 = format!(
        r#"{{"id":"{tag_id}","name":"CLI tag renamed","version":2,"created_at":"2026-08-29T12:00:00Z","updated_at":"2026-08-29T12:00:01Z"}}"#
    );
    assert_eq!(
        run_cli(&store, &["tag", "create", "--json", &tag_v1])["version"],
        1
    );
    assert_eq!(run_cli(&store, &["tag", "find", tag_id])["name"], "CLI tag");
    assert_eq!(
        run_cli(&store, &["tag", "list"]).as_array().unwrap().len(),
        1
    );
    assert_eq!(
        run_cli(
            &store,
            &[
                "tag",
                "replace",
                "--expected-version",
                "1",
                "--json",
                &tag_v2
            ]
        )["version"],
        2
    );
}
