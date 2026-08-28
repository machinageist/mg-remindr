use chrono::{TimeZone, Utc};
use mg_todo::{
    config::{Config, DatabaseUrl},
    domain::{Lifecycle, Project, ProjectId, Version},
    storage::{PostgresProjectRepository, StorageError, migrate, migration_status},
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

#[tokio::test]
async fn disposable_postgres_proves_migrations_and_optimistic_project_writes() {
    if !Config::integration_tests_enabled() {
        return;
    }
    let database = DisposablePostgres::start();

    let pending = migration_status(&database.url).await.unwrap();
    assert!(pending.iter().all(|state| !state.applied));
    assert!(
        migrate(&database.url)
            .await
            .unwrap()
            .iter()
            .all(|state| state.applied)
    );
    assert!(
        migrate(&database.url)
            .await
            .unwrap()
            .iter()
            .all(|state| state.applied)
    );

    let repository = PostgresProjectRepository::new(database.url.clone());
    let original = project(Lifecycle::Open, 1);
    repository.create(&original).await.unwrap();
    assert!(matches!(
        repository.create(&original).await,
        Err(StorageError::ProjectAlreadyExists { .. })
    ));
    assert_eq!(
        repository.find(original.id()).await.unwrap(),
        Some(original)
    );

    let completed = project(Lifecycle::Completed, 2);
    repository
        .replace(Version::new(), &completed)
        .await
        .unwrap();
    assert_eq!(
        repository.find(completed.id()).await.unwrap(),
        Some(completed.clone())
    );

    let invalid = project(Lifecycle::Trashed, 3);
    assert!(matches!(
        repository.replace(completed.version(), &invalid).await,
        Err(StorageError::Domain(_))
    ));
    assert_eq!(
        repository.find(completed.id()).await.unwrap(),
        Some(completed.clone())
    );

    let reopen = project(Lifecycle::Open, 3);
    let first = repository.clone();
    let second = repository.clone();
    let expected = completed.version();
    let (left, right) = tokio::join!(
        first.replace(expected, &reopen),
        second.replace(expected, &reopen)
    );
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
    assert_eq!(repository.find(reopen.id()).await.unwrap(), Some(reopen));

    let (client, connection) = tokio_postgres::connect(&database.raw_url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .execute(
            "INSERT INTO mg_todo_schema_migrations (version, name) VALUES ($1, $2)",
            &[&99_i64, &"future_schema"],
        )
        .await
        .unwrap();
    assert!(matches!(
        migration_status(&database.url).await,
        Err(StorageError::UnknownMigration { version: 99, .. })
    ));
    assert!(matches!(
        migrate(&database.url).await,
        Err(StorageError::UnknownMigration { version: 99, .. })
    ));
}
