// Author: Jeff
// Date: 2026-09-20
// Description: The tag repository against a real store — migration, optimistic writes, and the
//              timestamp contract
// Notes: Each test owns a store in a throwaway directory, so tests never see each other's rows
//        and none of them can reach Jeff's. The old file span a disposable PostgreSQL for this;
//        a SQLite store is a file, so the harness is a temporary directory

use chrono::{TimeZone, Timelike, Utc};
use mg_remindr::{
    domain::{Tag, TagId, Version},
    storage::{StorageError, Store, TagRepository, migrate, migration_status},
};
use std::{path::PathBuf, thread};
use tempfile::TempDir;

// A store in a throwaway directory, returned with it because dropping it deletes the file
fn scratch() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("remindr.sqlite");
    (directory, path)
}

fn tag(id: u128, name: &str, version: u64) -> Tag {
    let created_at = Utc.with_ymd_and_hms(2026, 8, 28, 12, 0, 0).unwrap();
    Tag::new(
        TagId::from_uuid(uuid::Uuid::from_u128(id)),
        name.to_owned(),
        Version::try_from_value(version).unwrap(),
        created_at,
        created_at + chrono::Duration::seconds(i64::try_from(version).unwrap()),
    )
    .unwrap()
}

// The tables the migration is expected to leave behind
fn stored_tables(path: &std::path::Path) -> Vec<String> {
    let connection = rusqlite::Connection::open(path).expect("open the store");
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name IN ('schema_migrations', 'projects', 'tags') ORDER BY name",
        )
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_store_migrates_once_and_then_holds_tags_under_an_optimistic_lock() {
    let (_directory, path) = scratch();

    assert!(
        migration_status(&path)
            .unwrap()
            .iter()
            .all(|state| !state.applied)
    );
    // applying twice is the same as applying once
    for _ in 0..2 {
        assert!(migrate(&path).unwrap().iter().all(|state| state.applied));
    }
    assert_eq!(
        stored_tables(&path),
        ["projects", "schema_migrations", "tags"]
    );

    let repository = TagRepository::new(Store::open(&path).unwrap());
    let alpha = tag(2, "alpha", 1);
    let beta = tag(1, "beta", 1);
    repository.create(&alpha).unwrap();
    repository.create(&beta).unwrap();
    assert!(matches!(
        repository.create(&alpha),
        Err(StorageError::TagAlreadyExists { .. })
    ));
    assert_eq!(repository.find(alpha.id()).unwrap(), Some(alpha.clone()));
    assert_eq!(repository.find(TagId::new()).unwrap(), None);
    assert_eq!(repository.list().unwrap(), vec![beta, alpha.clone()]);

    let renamed = tag(2, "renamed", 2);
    repository.replace(alpha.version(), &renamed).unwrap();
    assert_eq!(repository.find(alpha.id()).unwrap(), Some(renamed.clone()));
    // the version the writer observed is spent, and a second write with it is a conflict
    assert!(matches!(
        repository.replace(alpha.version(), &renamed),
        Err(StorageError::TagVersionConflict {
            expected: 1,
            actual: 2,
            ..
        })
    ));
    assert!(matches!(
        repository.replace(Version::new(), &tag(99, "missing", 2)),
        Err(StorageError::TagNotFound { .. })
    ));
}

#[test]
fn a_stored_tag_that_no_longer_validates_is_refused_rather_than_returned() {
    let (_directory, path) = scratch();
    let repository = TagRepository::new(Store::open(&path).unwrap());
    let stored = tag(3, "named", 1);
    repository.create(&stored).unwrap();

    // a newline passes SQLite's trim(), so the row is storable and still invalid
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE tags SET name = ?1 WHERE id = ?2",
            rusqlite::params!["\n", stored.id().to_string()],
        )
        .unwrap();
    assert!(matches!(
        repository.find(stored.id()),
        Err(StorageError::InvalidStoredTagData)
    ));
}

#[test]
fn two_writers_holding_the_same_version_produce_one_winner() {
    let (_directory, path) = scratch();
    let repository = TagRepository::new(Store::open(&path).unwrap());
    let stored = tag(4, "contended", 1);
    repository.create(&stored).unwrap();
    let next = tag(4, "concurrent", 2);

    let expected = stored.version();
    let (first, second) = (repository.clone(), repository.clone());
    let (left, right) = {
        let one = next.clone();
        let two = next.clone();
        let left = thread::spawn(move || first.replace(expected, &one));
        let right = thread::spawn(move || second.replace(expected, &two));
        (left.join().unwrap(), right.join().unwrap())
    };

    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let conflict = if left.is_err() { left } else { right };
    assert!(matches!(
        conflict,
        Err(StorageError::TagVersionConflict {
            expected: 1,
            actual: 2,
            ..
        })
    ));
    assert_eq!(repository.find(next.id()).unwrap(), Some(next));
}

#[test]
fn a_tampered_ledger_checksum_is_refused_by_both_status_and_apply() {
    let (_directory, path) = scratch();
    migrate(&path).unwrap();

    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE schema_migrations SET checksum = ?1 WHERE version = ?2",
            rusqlite::params!["tampered", 1_i64],
        )
        .unwrap();

    assert!(matches!(
        migration_status(&path),
        Err(StorageError::MigrationChecksumDrift { version: 1 })
    ));
    assert!(matches!(
        migrate(&path),
        Err(StorageError::MigrationChecksumDrift { version: 1 })
    ));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario proves precision rejection, refusal, and recovery in order"
)]
fn timestamps_keep_microsecond_precision_and_a_refused_write_changes_nothing() {
    let (_directory, path) = scratch();
    let repository = TagRepository::new(Store::open(&path).unwrap());
    let created_at = Utc
        .with_ymd_and_hms(2026, 8, 28, 12, 0, 0)
        .unwrap()
        .with_nanosecond(123_456_000)
        .unwrap();
    let updated_at = created_at + chrono::Duration::seconds(1);

    let exact = Tag::new(
        TagId::from_uuid(uuid::Uuid::from_u128(0x100)),
        "exact".to_owned(),
        Version::new(),
        created_at,
        updated_at,
    )
    .unwrap();
    repository.create(&exact).unwrap();
    assert_eq!(repository.find(exact.id()).unwrap().unwrap(), exact);

    // anything finer than a microsecond would not survive the store, so it is refused
    for (id, created_at, updated_at, field) in [
        (
            0x101,
            created_at + chrono::Duration::nanoseconds(1),
            updated_at,
            "created_at",
        ),
        (
            0x102,
            created_at,
            updated_at + chrono::Duration::nanoseconds(1),
            "updated_at",
        ),
    ] {
        let inexact = Tag::new(
            TagId::from_uuid(uuid::Uuid::from_u128(id)),
            "inexact".to_owned(),
            Version::new(),
            created_at,
            updated_at,
        )
        .unwrap();
        assert_eq!(
            repository.create(&inexact),
            Err(StorageError::InvalidTimestampPrecision { field })
        );
        assert_eq!(repository.find(inexact.id()).unwrap(), None);
    }

    let persisted = repository.find(exact.id()).unwrap().unwrap();
    let replacement = Tag::new(
        persisted.id(),
        "from persisted state".to_owned(),
        persisted.version().next().unwrap(),
        persisted.created_at(),
        persisted.updated_at() + chrono::Duration::microseconds(1),
    )
    .unwrap();
    repository
        .replace(persisted.version(), &replacement)
        .unwrap();
    assert_eq!(
        repository.find(replacement.id()).unwrap(),
        Some(replacement.clone())
    );

    let inexact_replacement = Tag::new(
        replacement.id(),
        "inexact replacement".to_owned(),
        replacement.version().next().unwrap(),
        replacement.created_at(),
        replacement.updated_at() + chrono::Duration::nanoseconds(1),
    )
    .unwrap();
    assert_eq!(
        repository.replace(replacement.version(), &inexact_replacement),
        Err(StorageError::InvalidTimestampPrecision {
            field: "updated_at"
        })
    );

    let changed_created_at = Tag::new(
        replacement.id(),
        "changed history".to_owned(),
        replacement.version().next().unwrap(),
        replacement.created_at() + chrono::Duration::microseconds(1),
        replacement.updated_at() + chrono::Duration::microseconds(2),
    )
    .unwrap();
    assert_eq!(
        repository.replace(replacement.version(), &changed_created_at),
        Err(StorageError::InvalidTagReplacement {
            reason: "created_at is immutable"
        })
    );
    assert_eq!(
        repository.find(replacement.id()).unwrap(),
        Some(replacement.clone())
    );

    let backward = Tag::new(
        replacement.id(),
        "backward".to_owned(),
        replacement.version().next().unwrap(),
        replacement.created_at(),
        replacement.updated_at() - chrono::Duration::microseconds(1),
    )
    .unwrap();
    assert_eq!(
        repository.replace(replacement.version(), &backward),
        Err(StorageError::InvalidTagReplacement {
            reason: "updated_at must not move backward"
        })
    );
    assert_eq!(
        repository.find(replacement.id()).unwrap(),
        Some(replacement.clone())
    );

    // the refusals left the row and its version alone, so the write that follows still fits
    let after_failures = Tag::new(
        replacement.id(),
        "after rollback".to_owned(),
        replacement.version().next().unwrap(),
        replacement.created_at(),
        replacement.updated_at() + chrono::Duration::microseconds(2),
    )
    .unwrap();
    repository
        .replace(replacement.version(), &after_failures)
        .unwrap();
    assert_eq!(
        repository.find(after_failures.id()).unwrap(),
        Some(after_failures)
    );

    let overflow = Tag::new(
        TagId::from_uuid(uuid::Uuid::from_u128(0x103)),
        "overflow".to_owned(),
        Version::try_from_value(u64::MAX).unwrap(),
        created_at,
        updated_at,
    )
    .unwrap();
    assert_eq!(
        repository.create(&overflow),
        Err(StorageError::InvalidTagCreation {
            reason: "version exceeds the stored integer range"
        })
    );
    assert_eq!(repository.find(overflow.id()).unwrap(), None);
}
