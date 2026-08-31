use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

#[test]
fn help_exposes_persistence_workflows_without_database_configuration() {
    cargo_bin_cmd!("mg-todo")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("migration"))
        .stdout(predicate::str::contains("project"))
        .stdout(predicate::str::contains("tag"))
        .stdout(predicate::str::contains("todo"));

    cargo_bin_cmd!("mg-todo")
        .args(["project", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("create"))
        .stdout(predicate::str::contains("find"))
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("replace"));

    cargo_bin_cmd!("mg-todo")
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
    cargo_bin_cmd!("mg-todo")
        .arg("unknown")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unrecognized subcommand 'unknown'",
        ));

    cargo_bin_cmd!("mg-todo")
        .args(["project", "find", "not-a-uuid"])
        .env("MG_TODO_DATABASE_URL", "postgres://localhost/mg_todo")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "mg-todo: invalid project identifier",
        ));
}

#[test]
fn persistence_command_without_database_url_fails_nonzero() {
    cargo_bin_cmd!("mg-todo")
        .args(["migration", "status"])
        .env_remove("MG_TODO_DATABASE_URL")
        .env("XDG_CONFIG_HOME", "/nonexistent/mg-todo-test-config")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "mg-todo: database URL is required",
        ));
}
