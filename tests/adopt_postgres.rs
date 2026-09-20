// Author: Jeff
// Date: 2026-09-20
// Description: Adoption of the retired PostgreSQL rows — what it carries, and what it refuses
// Notes: The rows are written as `psql` emits them, so the fixtures here are the column names
//        the old database used, not the domain's own JSON

use mg_remindr::{
    adopt::{AdoptError, adopt_postgres_rows},
    domain::{Lifecycle, TodoDue},
    storage::{Store, TodoRepository},
};
use tempfile::TempDir;

// One todo row as PostgreSQL held it
fn todo_row(id: &str, title: &str, version: u64) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "title": title,
        "project_id": null,
        "lifecycle": "open",
        "version": version,
        "created_at": "2026-09-07T12:00:00+00:00",
        "updated_at": "2026-09-08T12:00:00+00:00",
        "completed_at": null,
        "trashed_at": null,
        "due_date": "2026-12-20",
        "due_at": null,
        "due_timezone": "America/Los_Angeles",
    })
}

fn scratch() -> (TempDir, Store) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = Store::open(directory.path().join("remindr.sqlite")).expect("store opens");
    (directory, store)
}

#[test]
fn adopted_rows_keep_their_identity_version_history_and_due_value() {
    let (_directory, store) = scratch();
    let export = serde_json::json!({
        "todos": [
            todo_row("00000000-0000-0000-0000-0000000000a1", "Pay rent", 3),
            todo_row("00000000-0000-0000-0000-0000000000a2", "Book the exam", 1),
        ],
        "authority_revision": 412,
    })
    .to_string();

    let adopted = adopt_postgres_rows(&store, &export).expect("the rows adopt");

    assert_eq!(adopted.todos, 2);
    assert_eq!(adopted.projects, 0);
    // a consumer that remembers the old checkpoint is never handed a lower one
    assert_eq!(adopted.revision, 412);

    let stored = TodoRepository::new(store.clone())
        .list()
        .expect("todos load");
    assert_eq!(stored.len(), 2);
    let rent = stored
        .iter()
        .find(|todo| todo.title() == "Pay rent")
        .expect("the reminder is there");
    assert_eq!(rent.version().value(), 3, "the version is not restarted");
    assert_eq!(rent.lifecycle(), Lifecycle::Open);
    assert_eq!(
        rent.created_at().to_rfc3339(),
        "2026-09-07T12:00:00+00:00",
        "the history it already had is kept"
    );
    assert!(matches!(
        rent.due(),
        Some(TodoDue::Date { timezone, .. }) if timezone == "America/Los_Angeles"
    ));
}

#[test]
fn a_store_that_already_holds_reminders_is_left_alone() {
    let (_directory, store) = scratch();
    let export =
        serde_json::json!({ "todos": [todo_row("00000000-0000-0000-0000-0000000000b1", "First", 1)] })
            .to_string();
    adopt_postgres_rows(&store, &export).expect("the first adoption works");

    let second = serde_json::json!({
        "todos": [todo_row("00000000-0000-0000-0000-0000000000b2", "Second", 1)],
    })
    .to_string();
    assert!(matches!(
        adopt_postgres_rows(&store, &second),
        Err(AdoptError::StoreNotEmpty)
    ));
    assert_eq!(
        TodoRepository::new(store.clone()).list().unwrap().len(),
        1,
        "the refusal wrote nothing"
    );
}

#[test]
fn an_export_carrying_a_table_this_path_cannot_hold_is_refused_whole() {
    let (_directory, store) = scratch();
    let export = serde_json::json!({
        "todos": [todo_row("00000000-0000-0000-0000-0000000000c1", "Tagged", 1)],
        "todo_tags": [{"todo_id": "00000000-0000-0000-0000-0000000000c1",
                       "tag_id": "00000000-0000-0000-0000-0000000000c2"}],
    })
    .to_string();

    assert!(matches!(
        adopt_postgres_rows(&store, &export),
        Err(AdoptError::UnsupportedTable { table: "todo_tags" })
    ));
    assert!(
        TodoRepository::new(store.clone())
            .list()
            .unwrap()
            .is_empty(),
        "nothing is adopted when part of the export cannot be"
    );
}

#[test]
fn a_row_the_domain_refuses_stops_the_whole_adoption() {
    let (_directory, store) = scratch();
    let mut row = todo_row("00000000-0000-0000-0000-0000000000d1", "Backwards", 1);
    row["updated_at"] = serde_json::json!("2026-09-06T12:00:00+00:00");
    let export = serde_json::json!({
        "todos": [todo_row("00000000-0000-0000-0000-0000000000d2", "Fine", 1), row],
    })
    .to_string();

    assert!(matches!(
        adopt_postgres_rows(&store, &export),
        Err(AdoptError::InvalidRow { kind: "todo", .. })
    ));
    assert!(
        TodoRepository::new(store).list().unwrap().is_empty(),
        "the good row ahead of the bad one is not written either"
    );
}

#[test]
fn an_unreadable_export_is_refused_before_anything_is_read_from_it() {
    let (_directory, store) = scratch();
    assert!(matches!(
        adopt_postgres_rows(&store, "not json"),
        Err(AdoptError::Unreadable)
    ));
    // a column the old database never had means the export was not written by psql
    let surprising = serde_json::json!({
        "todos": [{"id": "00000000-0000-0000-0000-0000000000e1", "title": "Odd",
                   "lifecycle": "open", "version": 1,
                   "created_at": "2026-09-07T12:00:00+00:00",
                   "updated_at": "2026-09-07T12:00:00+00:00", "priority": 3}],
    })
    .to_string();
    assert!(matches!(
        adopt_postgres_rows(&store, &surprising),
        Err(AdoptError::Unreadable)
    ));
}
