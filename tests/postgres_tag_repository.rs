use chrono::{TimeZone, Timelike, Utc};
use mg_todo::{
    config::{Config, DatabaseUrl},
    domain::{Tag, TagId, Version},
    storage::{
        FOUNDATION_MIGRATION, MIGRATIONS, PostgresTagRepository, StorageError, migrate,
        migration_status,
    },
};
use std::{
    net::TcpListener,
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};
use tempfile::TempDir;

struct DisposablePostgres {
    _directory: TempDir,
    child: Child,
    raw_url: String,
    url: DatabaseUrl,
}

impl DisposablePostgres {
    fn start() -> Self {
        let directory = tempfile::tempdir().expect("create PostgreSQL test directory");
        let data = directory.path().join("data");
        let status = Command::new("initdb")
            .args(["-A", "trust", "-U", "postgres", "--no-sync", "-D"])
            .arg(&data)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run initdb");
        assert!(status.success(), "initdb failed");

        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve local test port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let socket_dir = directory.path().join("socket");
        std::fs::create_dir(&socket_dir).unwrap();
        let child = Command::new("postgres")
            .args(["-F", "-h", "127.0.0.1", "-p", &port.to_string(), "-k"])
            .arg(&socket_dir)
            .arg("-D")
            .arg(&data)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start disposable PostgreSQL");
        let raw_url = format!("postgres://postgres@127.0.0.1:{port}/postgres");
        let url = DatabaseUrl::parse(raw_url.clone()).unwrap();
        let ready = (0..100).any(|_| {
            let ready = Command::new("pg_isready")
                .args(["-h", "127.0.0.1", "-p", &port.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if !ready {
                thread::sleep(Duration::from_millis(25));
            }
            ready
        });
        assert!(ready, "disposable PostgreSQL did not become ready");
        Self {
            _directory: directory,
            child,
            raw_url,
            url,
        }
    }
}

impl Drop for DisposablePostgres {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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

async fn assert_current_schema_tables(client: &tokio_postgres::Client) {
    let tables = client
        .query(
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = current_schema() \
             AND table_name = ANY($1) ORDER BY table_name",
            &[&vec!["mg_todo_schema_migrations", "projects", "tags"]],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    assert_eq!(tables, ["mg_todo_schema_migrations", "projects", "tags"]);
}

async fn assert_invalid_stored_tag_is_rejected(
    client: &tokio_postgres::Client,
    repository: &PostgresTagRepository,
    tag: &Tag,
) {
    client
        .execute(
            "UPDATE tags SET name = $1 WHERE id = $2",
            &[&"\n", &tag.id().as_uuid()],
        )
        .await
        .unwrap();
    assert!(matches!(
        repository.find(tag.id()).await,
        Err(StorageError::InvalidStoredTagData)
    ));
    client
        .execute(
            "UPDATE tags SET name = $1 WHERE id = $2",
            &[&tag.name(), &tag.id().as_uuid()],
        )
        .await
        .unwrap();
}

async fn connect_test_database(database: &DisposablePostgres) -> tokio_postgres::Client {
    let (client, connection) = tokio_postgres::connect(&database.raw_url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

fn assert_migration_gap(error: &StorageError) {
    assert_eq!(
        error,
        &StorageError::MigrationHistoryGap {
            missing_version: 1,
            applied_version: 2,
        }
    );
    assert_eq!(
        error.to_string(),
        "migration history gap: migration 2 is applied before migration 1"
    );
}

#[tokio::test]
async fn migration_ledger_with_only_v2_fails_closed_before_legacy_ledger_mutation() {
    if !Config::integration_tests_enabled() {
        return;
    }
    let database = DisposablePostgres::start();
    let client = connect_test_database(&database).await;
    client
        .batch_execute(
            "CREATE TABLE mg_todo_schema_migrations (\
             version bigint PRIMARY KEY, \
             name text NOT NULL, \
             applied_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP); \
             INSERT INTO mg_todo_schema_migrations (version, name) \
             VALUES (2, 'tag_authority')",
        )
        .await
        .unwrap();

    assert_migration_gap(&migration_status(&database.url).await.unwrap_err());
    assert_migration_gap(&migrate(&database.url).await.unwrap_err());

    let versions = client
        .query(
            "SELECT version FROM mg_todo_schema_migrations ORDER BY version",
            &[],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<_, i64>(0))
        .collect::<Vec<_>>();
    assert_eq!(versions, [2]);
    let checksum_added: bool = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
             WHERE table_schema = current_schema() \
             AND table_name = 'mg_todo_schema_migrations' \
             AND column_name = 'checksum')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(!checksum_added);
    let application_tables: i64 = client
        .query_one(
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_schema = current_schema() \
             AND table_name IN ('projects', 'tags')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(application_tables, 0);
}

#[tokio::test]
async fn checksum_ledger_with_deleted_v1_is_rejected_without_reapplying_v1() {
    if !Config::integration_tests_enabled() {
        return;
    }
    let database = DisposablePostgres::start();
    migrate(&database.url).await.unwrap();
    let client = connect_test_database(&database).await;
    client
        .batch_execute(
            "DELETE FROM mg_todo_schema_migrations WHERE version = 1; \
             DROP TABLE projects CASCADE",
        )
        .await
        .unwrap();

    assert_migration_gap(&migration_status(&database.url).await.unwrap_err());
    assert_migration_gap(&migrate(&database.url).await.unwrap_err());

    let versions = client
        .query(
            "SELECT version FROM mg_todo_schema_migrations ORDER BY version",
            &[],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<_, i64>(0))
        .collect::<Vec<_>>();
    // Every ledger row except the deleted foundation entry must survive untouched
    let retained = MIGRATIONS
        .iter()
        .map(|migration| migration.version)
        .filter(|version| *version != 1)
        .collect::<Vec<_>>();
    assert_eq!(versions, retained);
    let projects_exists: bool = client
        .query_one("SELECT to_regclass('projects') IS NOT NULL", &[])
        .await
        .unwrap()
        .get(0);
    assert!(!projects_exists);
}

#[tokio::test]
async fn disposable_postgres_proves_tag_migration_and_optimistic_writes() {
    if !Config::integration_tests_enabled() {
        return;
    }
    let database = DisposablePostgres::start();

    assert!(
        migration_status(&database.url)
            .await
            .unwrap()
            .iter()
            .all(|state| !state.applied)
    );
    for _ in 0..2 {
        assert!(
            migrate(&database.url)
                .await
                .unwrap()
                .iter()
                .all(|state| state.applied)
        );
    }

    let client = connect_test_database(&database).await;
    assert_current_schema_tables(&client).await;

    let repository = PostgresTagRepository::new(database.url.clone());
    let alpha = tag(2, "alpha", 1);
    let beta = tag(1, "beta", 1);
    repository.create(&alpha).await.unwrap();
    repository.create(&beta).await.unwrap();
    assert!(matches!(
        repository.create(&alpha).await,
        Err(StorageError::TagAlreadyExists { .. })
    ));
    assert_eq!(
        repository.find(alpha.id()).await.unwrap(),
        Some(alpha.clone())
    );
    assert_eq!(repository.find(TagId::new()).await.unwrap(), None);
    assert_eq!(repository.list().await.unwrap(), vec![beta, alpha.clone()]);

    let renamed = tag(2, "renamed", 2);
    repository.replace(alpha.version(), &renamed).await.unwrap();
    assert_eq!(
        repository.find(alpha.id()).await.unwrap(),
        Some(renamed.clone())
    );
    assert!(matches!(
        repository.replace(alpha.version(), &renamed).await,
        Err(StorageError::TagVersionConflict {
            expected: 1,
            actual: 2,
            ..
        })
    ));
    assert!(matches!(
        repository
            .replace(Version::new(), &tag(99, "missing", 2))
            .await,
        Err(StorageError::TagNotFound { .. })
    ));

    assert_invalid_stored_tag_is_rejected(&client, &repository, &renamed).await;

    let concurrent = tag(2, "concurrent", 3);
    let first = repository.clone();
    let second = repository.clone();
    let expected = renamed.version();
    let (left, right) = tokio::join!(
        first.replace(expected, &concurrent),
        second.replace(expected, &concurrent)
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let conflict = if left.is_err() { left } else { right };
    assert!(matches!(
        conflict,
        Err(StorageError::TagVersionConflict {
            expected: 2,
            actual: 3,
            ..
        })
    ));
    assert_eq!(
        repository.find(concurrent.id()).await.unwrap(),
        Some(concurrent)
    );

    client
        .execute(
            "UPDATE mg_todo_schema_migrations SET checksum = $1 WHERE version = $2",
            &[&"tampered", &2_i64],
        )
        .await
        .unwrap();
    assert!(matches!(
        migration_status(&database.url).await,
        Err(StorageError::MigrationChecksumDrift { version: 2 })
    ));
    assert!(matches!(
        migrate(&database.url).await,
        Err(StorageError::MigrationChecksumDrift { version: 2 })
    ));
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one transactional scenario proves precision rejection, rollback, and recovery"
)]
async fn timestamps_obey_postgres_precision_and_post_lock_failures_roll_back() {
    if !Config::integration_tests_enabled() {
        return;
    }
    let database = DisposablePostgres::start();
    migrate(&database.url).await.unwrap();
    let repository = PostgresTagRepository::new(database.url.clone());
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
    repository.create(&exact).await.unwrap();
    let persisted = repository.find(exact.id()).await.unwrap().unwrap();
    assert_eq!(persisted, exact);

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
            repository.create(&inexact).await,
            Err(StorageError::InvalidTimestampPrecision { field })
        );
        assert_eq!(repository.find(inexact.id()).await.unwrap(), None);
    }

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
        .await
        .unwrap();
    assert_eq!(
        repository.find(replacement.id()).await.unwrap(),
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
        repository
            .replace(replacement.version(), &inexact_replacement)
            .await,
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
        repository
            .replace(replacement.version(), &changed_created_at)
            .await,
        Err(StorageError::InvalidTagReplacement {
            reason: "created_at is immutable"
        })
    );
    assert_eq!(
        repository.find(replacement.id()).await.unwrap(),
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
        repository.replace(replacement.version(), &backward).await,
        Err(StorageError::InvalidTagReplacement {
            reason: "updated_at must not move backward"
        })
    );
    assert_eq!(
        repository.find(replacement.id()).await.unwrap(),
        Some(replacement.clone())
    );

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
        .await
        .unwrap();
    assert_eq!(
        repository.find(after_failures.id()).await.unwrap(),
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
        repository.create(&overflow).await,
        Err(StorageError::InvalidTagCreation {
            reason: "version exceeds PostgreSQL bigint range"
        })
    );
    assert_eq!(repository.find(overflow.id()).await.unwrap(), None);
}

#[tokio::test]
async fn legacy_v1_ledger_is_upgraded_with_trusted_checksum() {
    if !Config::integration_tests_enabled() {
        return;
    }
    let database = DisposablePostgres::start();
    let client = connect_test_database(&database).await;
    client.batch_execute(FOUNDATION_MIGRATION).await.unwrap();
    client
        .batch_execute(
            "CREATE TABLE mg_todo_schema_migrations (\
             version bigint PRIMARY KEY, name text NOT NULL, \
             applied_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP); \
             INSERT INTO mg_todo_schema_migrations (version, name) \
             VALUES (1, 'project_authority')",
        )
        .await
        .unwrap();

    migrate(&database.url).await.unwrap();
    let checksum: String = client
        .query_one(
            "SELECT checksum FROM mg_todo_schema_migrations WHERE version = 1",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(checksum, MIGRATIONS[0].checksum);
    assert_eq!(
        checksum,
        "0c821017adbb6c219ad371a7729b7d898a178bd9102279d71524504710ed78c0"
    );
}

#[tokio::test]
async fn malformed_unledgered_table_is_rejected_without_ledger_write() {
    if !Config::integration_tests_enabled() {
        return;
    }
    let database = DisposablePostgres::start();
    let client = connect_test_database(&database).await;
    client
        .batch_execute("CREATE TABLE projects (id uuid PRIMARY KEY)")
        .await
        .unwrap();

    for error in [
        migration_status(&database.url).await.unwrap_err(),
        migrate(&database.url).await.unwrap_err(),
    ] {
        assert_eq!(
            error,
            StorageError::UnledgeredMigrationTable { table: "projects" }
        );
    }
    let ledger_exists: bool = client
        .query_one(
            "SELECT to_regclass('mg_todo_schema_migrations') IS NOT NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(!ledger_exists);
}

#[tokio::test]
async fn recorded_migration_schema_drift_is_rejected() {
    if !Config::integration_tests_enabled() {
        return;
    }
    let database = DisposablePostgres::start();
    migrate(&database.url).await.unwrap();
    let client = connect_test_database(&database).await;
    client
        .batch_execute("ALTER TABLE tags DROP COLUMN name")
        .await
        .unwrap();

    for error in [
        migration_status(&database.url).await.unwrap_err(),
        migrate(&database.url).await.unwrap_err(),
    ] {
        assert_eq!(
            error,
            StorageError::MigrationSchemaDrift {
                version: 2,
                table: "tags"
            }
        );
    }
}

#[tokio::test]
async fn weakened_recorded_constraints_fail_closed_without_ledger_mutation() {
    if !Config::integration_tests_enabled() {
        return;
    }
    let database = DisposablePostgres::start();
    migrate(&database.url).await.unwrap();
    let client = connect_test_database(&database).await;

    let mutations = [
        (
            "tags_version_check",
            "CHECK (version >= 1 OR true)",
            "CHECK (version >= 1)",
        ),
        (
            "tags_name_check",
            "CHECK (btrim(name) <> '' OR true)",
            "CHECK (btrim(name) <> '')",
        ),
        (
            "tags_name_check",
            "CHECK (btrim(name) <> ' ')",
            "CHECK (btrim(name) <> '')",
        ),
        (
            "tags_check",
            "CHECK (updated_at >= created_at OR true)",
            "CHECK (updated_at >= created_at)",
        ),
    ];

    for (constraint, weakened, restored) in mutations {
        client
            .batch_execute(&format!(
                "ALTER TABLE tags DROP CONSTRAINT {constraint}; \
                 ALTER TABLE tags ADD CONSTRAINT {constraint} {weakened}"
            ))
            .await
            .unwrap();
        let ledger_before: String = client
            .query_one(
                "SELECT jsonb_agg(to_jsonb(m) ORDER BY version)::text \
                 FROM mg_todo_schema_migrations m",
                &[],
            )
            .await
            .unwrap()
            .get(0);

        for error in [
            migration_status(&database.url).await.unwrap_err(),
            migrate(&database.url).await.unwrap_err(),
        ] {
            assert_eq!(
                error,
                StorageError::MigrationSchemaDrift {
                    version: 2,
                    table: "tags"
                }
            );
        }
        let ledger_after: String = client
            .query_one(
                "SELECT jsonb_agg(to_jsonb(m) ORDER BY version)::text \
                 FROM mg_todo_schema_migrations m",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(ledger_after, ledger_before);

        client
            .batch_execute(&format!(
                "ALTER TABLE tags DROP CONSTRAINT {constraint}; \
                 ALTER TABLE tags ADD CONSTRAINT {constraint} {restored}"
            ))
            .await
            .unwrap();
        migration_status(&database.url).await.unwrap();
    }
}

fn run_cli(database: &DisposablePostgres, arguments: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_mg-todo"))
        .args(arguments)
        .env("MG_TODO_DATABASE_URL", &database.raw_url)
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
    if !Config::integration_tests_enabled() {
        return;
    }
    let database = DisposablePostgres::start();
    let pending = run_cli(&database, &["migration", "status"]);
    assert!(
        pending
            .as_array()
            .unwrap()
            .iter()
            .all(|item| !item["applied"].as_bool().unwrap())
    );
    let applied = run_cli(&database, &["migration", "apply"]);
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
        run_cli(&database, &["project", "create", "--json", &project_v1])["version"],
        1
    );
    assert_eq!(
        run_cli(&database, &["project", "find", project_id])["name"],
        "CLI project"
    );
    assert_eq!(
        run_cli(&database, &["project", "list"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        run_cli(
            &database,
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
        run_cli(&database, &["tag", "create", "--json", &tag_v1])["version"],
        1
    );
    assert_eq!(
        run_cli(&database, &["tag", "find", tag_id])["name"],
        "CLI tag"
    );
    assert_eq!(
        run_cli(&database, &["tag", "list"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        run_cli(
            &database,
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
