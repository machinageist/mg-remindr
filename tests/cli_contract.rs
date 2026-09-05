use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

#[test]
fn help_exposes_persistence_workflows_without_database_configuration() {
    cargo_bin_cmd!("mg-remindr")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("migration"))
        .stdout(predicate::str::contains("project"))
        .stdout(predicate::str::contains("tag"))
        .stdout(predicate::str::contains("todo"))
        .stdout(predicate::str::contains("add"))
        .stdout(predicate::str::contains("ls"))
        .stdout(predicate::str::contains("done"))
        .stdout(predicate::str::contains("rm"))
        .stdout(predicate::str::contains("restore"));

    cargo_bin_cmd!("mg-remindr")
        .args(["project", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("create"))
        .stdout(predicate::str::contains("find"))
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("replace"));

    cargo_bin_cmd!("mg-remindr")
        .args(["todo", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("create"))
        .stdout(predicate::str::contains("find"))
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("replace"));
}

#[test]
fn unknown_and_invalid_commands_fail_with_stable_clap_errors() {
    cargo_bin_cmd!("mg-remindr")
        .arg("unknown")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unrecognized subcommand 'unknown'",
        ));

    cargo_bin_cmd!("mg-remindr")
        .args(["project", "find", "not-a-uuid"])
        .env("MG_REMINDR_DATABASE_URL", "postgres://localhost/mg_todo")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "mg-remindr: invalid project identifier",
        ));
}

#[test]
fn a_remote_database_url_is_refused_before_any_connection() {
    cargo_bin_cmd!("mg-remindr")
        .args(["migration", "status"])
        .env(
            "MG_REMINDR_DATABASE_URL",
            "postgres://db.example.com/mg_todo",
        )
        .env("XDG_CONFIG_HOME", "/nonexistent/mg-remindr-test-config")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "mg-remindr: invalid database configuration",
        ));
}

#[test]
fn the_human_surface_needs_no_json_uuid_version_or_timestamp() {
    cargo_bin_cmd!("mg-remindr")
        .args(["add", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--due"))
        .stdout(predicate::str::contains("--timezone"))
        .stdout(predicate::str::contains("YYYY-MM-DD"))
        .stdout(predicate::str::contains("--json"));

    for command in ["add", "ls", "done", "rm", "restore"] {
        let help = cargo_bin_cmd!("mg-remindr")
            .args([command, "--help"])
            .assert()
            .success();
        let stdout = String::from_utf8(help.get_output().stdout.clone()).unwrap();
        assert!(
            !stdout.contains("--expected-version"),
            "{command} should not ask for a version"
        );
    }
}

#[test]
fn an_unreadable_due_value_fails_before_touching_the_database() {
    cargo_bin_cmd!("mg-remindr")
        .args(["add", "Nope", "--due", "next thursday"])
        .env(
            "MG_REMINDR_DATABASE_URL",
            "postgres://localhost/mg_todo_absent",
        )
        .assert()
        .failure()
        .stderr(predicate::str::contains("mg-remindr: due must be today"));

    cargo_bin_cmd!("mg-remindr")
        .args([
            "add",
            "Nope",
            "--due",
            "today",
            "--timezone",
            "Mars/Olympus",
        ])
        .env(
            "MG_REMINDR_DATABASE_URL",
            "postgres://localhost/mg_todo_absent",
        )
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "mg-remindr: timezone is not a named IANA zone",
        ));
}
