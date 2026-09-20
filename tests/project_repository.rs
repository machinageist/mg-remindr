// Author: Jeff
// Date: 2026-09-20
// Description: The project repository against a real store — migration, optimistic writes, and
//              the refusals that leave a row untouched
// Notes: A store is one file, so each test takes a throwaway directory of its own. The old file
//        span a disposable PostgreSQL for the same coverage

use chrono::{TimeZone, Utc};
use mg_remindr::{
    domain::{Lifecycle, Project, ProjectId, Version},
    storage::{ProjectRepository, StorageError, Store, migrate, migration_status},
};
use std::{path::PathBuf, thread};
use tempfile::TempDir;

// A store in a throwaway directory, returned with it because dropping it deletes the file
fn scratch() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("remindr.sqlite");
    (directory, path)
}

fn project(lifecycle: Lifecycle, version: u64) -> Project {
    let created_at = Utc.with_ymd_and_hms(2026, 8, 27, 12, 0, 0).unwrap();
    Project::new(
        ProjectId::from_uuid(uuid::Uuid::from_u128(0x1234)),
        "Authority".to_owned(),
        lifecycle,
        Version::try_from_value(version).unwrap(),
        created_at,
        created_at + chrono::Duration::seconds(i64::try_from(version).unwrap()),
    )
    .unwrap()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario walks a project from creation through every refusal"
)]
fn a_store_migrates_and_then_holds_projects_under_an_optimistic_lock() {
    let (_directory, path) = scratch();

    assert!(
        migration_status(&path)
            .unwrap()
            .iter()
            .all(|state| !state.applied)
    );
    for _ in 0..2 {
        assert!(migrate(&path).unwrap().iter().all(|state| state.applied));
    }

    let repository = ProjectRepository::new(Store::open(&path).unwrap());
    let original = project(Lifecycle::Open, 1);
    repository.create(&original).unwrap();
    assert!(matches!(
        repository.create(&original),
        Err(StorageError::ProjectAlreadyExists { .. })
    ));
    assert_eq!(repository.find(original.id()).unwrap(), Some(original));

    let completed = project(Lifecycle::Completed, 2);
    repository.replace(Version::new(), &completed).unwrap();
    assert_eq!(
        repository.find(completed.id()).unwrap(),
        Some(completed.clone())
    );

    let backward = Project::new(
        completed.id(),
        "Backward timestamp".to_owned(),
        Lifecycle::Open,
        completed.version().next().unwrap(),
        completed.created_at(),
        completed.updated_at() - chrono::Duration::seconds(1),
    )
    .unwrap();
    assert_eq!(
        repository.replace(completed.version(), &backward),
        Err(StorageError::InvalidReplacement {
            reason: "updated_at must not move backward"
        })
    );
    assert_eq!(
        repository.find(completed.id()).unwrap(),
        Some(completed.clone())
    );

    // a transition the domain forbids is refused by the domain, not written and repaired later
    let invalid = project(Lifecycle::Trashed, 3);
    assert!(matches!(
        repository.replace(completed.version(), &invalid),
        Err(StorageError::Domain(_))
    ));
    assert_eq!(
        repository.find(completed.id()).unwrap(),
        Some(completed.clone())
    );

    let reopen = project(Lifecycle::Open, 3);
    let (first, second) = (repository.clone(), repository.clone());
    let expected = completed.version();
    let (left, right) = {
        let one = reopen.clone();
        let two = reopen.clone();
        let left = thread::spawn(move || first.replace(expected, &one));
        let right = thread::spawn(move || second.replace(expected, &two));
        (left.join().unwrap(), right.join().unwrap())
    };
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let conflict = if left.is_err() { left } else { right };
    assert!(matches!(
        conflict,
        Err(StorageError::VersionConflict {
            expected: 2,
            actual: 3,
            ..
        })
    ));
    assert_eq!(repository.find(reopen.id()).unwrap(), Some(reopen));
}

#[test]
fn a_ledger_row_this_build_cannot_explain_stops_both_status_and_apply() {
    let (_directory, path) = scratch();
    migrate(&path).unwrap();

    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "INSERT INTO schema_migrations (version, name, checksum, applied_at) \
             VALUES (99, 'future_schema', 'unknown', '1970-01-01T00:00:00.000000Z')",
            [],
        )
        .unwrap();

    assert!(matches!(
        migration_status(&path),
        Err(StorageError::UnknownMigration { version: 99, .. })
    ));
    assert!(matches!(
        migrate(&path),
        Err(StorageError::UnknownMigration { version: 99, .. })
    ));
}
