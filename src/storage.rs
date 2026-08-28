use crate::{
    config::{DatabaseUrl, validate_local_database_url},
    domain::{DomainError, Lifecycle, Project, ProjectId, Version},
};
use chrono::{DateTime, Utc};
use thiserror::Error;
use tokio_postgres::{Client, NoTls, Row, error::SqlState};

pub const FOUNDATION_MIGRATION: &str = include_str!("../migrations/0001_project_authority.sql");
const LEDGER: &str = "mg_todo_schema_migrations";
const MIGRATION_LOCK: i64 = 73_407_463_646;

#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "project_authority",
    sql: FOUNDATION_MIGRATION,
}];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationState {
    pub version: i64,
    pub name: String,
    pub applied: bool,
}

/// Stable storage failures. Driver errors are intentionally redacted so URLs and
/// credentials cannot enter display or debug output.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StorageError {
    #[error("database configuration is not local-only")]
    InvalidConfiguration,
    #[error("mg-todo database connection failed")]
    Connect,
    #[error("mg-todo database operation failed during {operation}")]
    Database { operation: &'static str },
    #[error("migration {version} drift: expected '{expected}', recorded '{actual}'")]
    MigrationDrift {
        version: i64,
        expected: &'static str,
        actual: String,
    },
    #[error("database contains unknown migration {version} named '{name}'")]
    UnknownMigration { version: i64, name: String },
    #[error("project {project_id} already exists")]
    ProjectAlreadyExists { project_id: ProjectId },
    #[error("project {project_id} was not found")]
    ProjectNotFound { project_id: ProjectId },
    #[error("project {project_id} version conflict: expected {expected}, actual {actual}")]
    VersionConflict {
        project_id: ProjectId,
        expected: u64,
        actual: u64,
    },
    #[error("invalid project replacement: {reason}")]
    InvalidReplacement { reason: &'static str },
    #[error("invalid stored project data")]
    InvalidStoredData,
    #[error(transparent)]
    Domain(#[from] DomainError),
}

async fn connect(database_url: &DatabaseUrl) -> Result<Client, StorageError> {
    validate_local_database_url(database_url.as_str())
        .map_err(|_| StorageError::InvalidConfiguration)?;
    let (client, connection) = tokio_postgres::connect(database_url.as_str(), NoTls)
        .await
        .map_err(|_| StorageError::Connect)?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

/// Read migration state without creating or changing the ledger.
pub async fn migration_status(
    database_url: &DatabaseUrl,
) -> Result<Vec<MigrationState>, StorageError> {
    let client = connect(database_url).await?;
    let exists: bool = client
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&LEDGER])
        .await
        .map_err(|_| StorageError::Database {
            operation: "migration status",
        })?
        .get(0);
    if !exists {
        return Ok(MIGRATIONS
            .iter()
            .map(|migration| MigrationState {
                version: migration.version,
                name: migration.name.to_owned(),
                applied: false,
            })
            .collect());
    }

    let rows = client
        .query(
            "SELECT version, name FROM mg_todo_schema_migrations ORDER BY version",
            &[],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "migration status",
        })?;
    validate_recorded_migrations(&rows)?;
    MIGRATIONS
        .iter()
        .map(|migration| {
            let recorded = rows
                .iter()
                .find(|row| row.get::<_, i64>(0) == migration.version);
            if let Some(row) = recorded {
                let actual = row.get::<_, String>(1);
                if actual != migration.name {
                    return Err(StorageError::MigrationDrift {
                        version: migration.version,
                        expected: migration.name,
                        actual,
                    });
                }
            }
            Ok(MigrationState {
                version: migration.version,
                name: migration.name.to_owned(),
                applied: recorded.is_some(),
            })
        })
        .collect()
}

/// Apply all pending embedded migrations under a transaction-scoped advisory lock.
/// Schema SQL and its ledger row commit atomically; rerunning is idempotent.
pub async fn migrate(database_url: &DatabaseUrl) -> Result<Vec<MigrationState>, StorageError> {
    let mut client = connect(database_url).await?;
    let transaction = client
        .transaction()
        .await
        .map_err(|_| StorageError::Database {
            operation: "migration",
        })?;
    transaction
        .query_one("SELECT pg_advisory_xact_lock($1)", &[&MIGRATION_LOCK])
        .await
        .map_err(|_| StorageError::Database {
            operation: "migration",
        })?;
    transaction
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS mg_todo_schema_migrations (\
             version bigint PRIMARY KEY, \
             name text NOT NULL, \
             applied_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "migration",
        })?;

    let recorded = transaction
        .query(
            "SELECT version, name FROM mg_todo_schema_migrations ORDER BY version",
            &[],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "migration",
        })?;
    validate_recorded_migrations(&recorded)?;

    for migration in MIGRATIONS {
        let existing = transaction
            .query_opt(
                "SELECT name FROM mg_todo_schema_migrations WHERE version = $1",
                &[&migration.version],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "migration",
            })?;
        if let Some(row) = existing {
            let actual = row.get::<_, String>(0);
            if actual != migration.name {
                return Err(StorageError::MigrationDrift {
                    version: migration.version,
                    expected: migration.name,
                    actual,
                });
            }
            continue;
        }
        transaction
            .batch_execute(migration.sql)
            .await
            .map_err(|_| StorageError::Database {
                operation: "migration",
            })?;
        transaction
            .execute(
                "INSERT INTO mg_todo_schema_migrations (version, name) VALUES ($1, $2)",
                &[&migration.version, &migration.name],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "migration",
            })?;
    }
    transaction
        .commit()
        .await
        .map_err(|_| StorageError::Database {
            operation: "migration",
        })?;
    migration_status(database_url).await
}

fn validate_recorded_migrations(rows: &[Row]) -> Result<(), StorageError> {
    for row in rows {
        let version = row.get::<_, i64>(0);
        let name = row.get::<_, String>(1);
        let Some(expected) = MIGRATIONS
            .iter()
            .find(|migration| migration.version == version)
        else {
            return Err(StorageError::UnknownMigration { version, name });
        };
        if name != expected.name {
            return Err(StorageError::MigrationDrift {
                version,
                expected: expected.name,
                actual: name,
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PostgresProjectRepository {
    database_url: DatabaseUrl,
}

impl PostgresProjectRepository {
    #[must_use]
    pub const fn new(database_url: DatabaseUrl) -> Self {
        Self { database_url }
    }

    /// Insert a validated project without rewriting caller-owned identity or history.
    pub async fn create(&self, project: &Project) -> Result<(), StorageError> {
        revalidate(project)?;
        let client = connect(&self.database_url).await?;
        let version = database_version(project.version())?;
        let lifecycle = lifecycle_text(project.lifecycle());
        let result = client
            .execute(
                "INSERT INTO projects \
                 (id, name, lifecycle, version, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &project.id().as_uuid(),
                    &project.name(),
                    &lifecycle,
                    &version,
                    &project.created_at(),
                    &project.updated_at(),
                ],
            )
            .await;
        match result {
            Ok(1) => Ok(()),
            Ok(_) => Err(StorageError::Database {
                operation: "project create",
            }),
            Err(error) if error.code() == Some(&SqlState::UNIQUE_VIOLATION) => {
                Err(StorageError::ProjectAlreadyExists {
                    project_id: project.id(),
                })
            }
            Err(_) => Err(StorageError::Database {
                operation: "project create",
            }),
        }
    }

    pub async fn find(&self, project_id: ProjectId) -> Result<Option<Project>, StorageError> {
        let client = connect(&self.database_url).await?;
        client
            .query_opt(
                "SELECT id, name, lifecycle, version, created_at, updated_at \
                 FROM projects WHERE id = $1",
                &[&project_id.as_uuid()],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "project find",
            })?
            .as_ref()
            .map(project_from_row)
            .transpose()
    }

    /// List every project in deterministic ID order without changing authority.
    pub async fn list(&self) -> Result<Vec<Project>, StorageError> {
        let client = connect(&self.database_url).await?;
        let rows = client
            .query(
                "SELECT id, name, lifecycle, version, created_at, updated_at \
                 FROM projects ORDER BY id",
                &[],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "project list",
            })?;
        rows.iter().map(project_from_row).collect()
    }

    /// Replace one project under a row lock and caller-supplied optimistic version.
    /// Lifecycle validity is checked before the write and before future dependent checks.
    pub async fn replace(
        &self,
        expected: Version,
        replacement: &Project,
    ) -> Result<(), StorageError> {
        revalidate(replacement)?;
        let mut client = connect(&self.database_url).await?;
        let transaction = client
            .transaction()
            .await
            .map_err(|_| StorageError::Database {
                operation: "project replace",
            })?;
        let row = transaction
            .query_opt(
                "SELECT id, name, lifecycle, version, created_at, updated_at \
                 FROM projects WHERE id = $1 FOR UPDATE",
                &[&replacement.id().as_uuid()],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "project replace",
            })?
            .ok_or(StorageError::ProjectNotFound {
                project_id: replacement.id(),
            })?;
        let current = project_from_row(&row)?;
        if current.version() != expected {
            return Err(StorageError::VersionConflict {
                project_id: replacement.id(),
                expected: expected.value(),
                actual: current.version().value(),
            });
        }
        current.lifecycle().transition(replacement.lifecycle())?;
        if replacement.version() != expected.next()? {
            return Err(StorageError::InvalidReplacement {
                reason: "replacement version must be expected version plus one",
            });
        }
        if replacement.created_at() != current.created_at() {
            return Err(StorageError::InvalidReplacement {
                reason: "created_at is immutable",
            });
        }
        if replacement.updated_at() < current.updated_at() {
            return Err(StorageError::InvalidReplacement {
                reason: "updated_at must not move backward",
            });
        }
        let version = database_version(replacement.version())?;
        let lifecycle = lifecycle_text(replacement.lifecycle());
        let changed = transaction
            .execute(
                "UPDATE projects SET name = $1, lifecycle = $2, version = $3, updated_at = $4 \
                 WHERE id = $5 AND version = $6",
                &[
                    &replacement.name(),
                    &lifecycle,
                    &version,
                    &replacement.updated_at(),
                    &replacement.id().as_uuid(),
                    &database_version(expected)?,
                ],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "project replace",
            })?;
        if changed != 1 {
            return Err(StorageError::Database {
                operation: "project replace",
            });
        }
        transaction
            .commit()
            .await
            .map_err(|_| StorageError::Database {
                operation: "project replace",
            })
    }
}

fn revalidate(project: &Project) -> Result<(), StorageError> {
    Project::new(
        project.id(),
        project.name().to_owned(),
        project.lifecycle(),
        project.version(),
        project.created_at(),
        project.updated_at(),
    )?;
    Ok(())
}

fn database_version(version: Version) -> Result<i64, StorageError> {
    i64::try_from(version.value()).map_err(|_| StorageError::InvalidReplacement {
        reason: "version exceeds PostgreSQL bigint range",
    })
}

const fn lifecycle_text(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Open => "open",
        Lifecycle::Completed => "completed",
        Lifecycle::Trashed => "trashed",
    }
}

fn project_from_row(row: &Row) -> Result<Project, StorageError> {
    let lifecycle = match row.get::<_, &str>(2) {
        "open" => Lifecycle::Open,
        "completed" => Lifecycle::Completed,
        "trashed" => Lifecycle::Trashed,
        _ => return Err(StorageError::InvalidStoredData),
    };
    let raw_version = row.get::<_, i64>(3);
    let version = u64::try_from(raw_version)
        .ok()
        .and_then(|value| Version::try_from_value(value).ok())
        .ok_or(StorageError::InvalidStoredData)?;
    Project::new(
        ProjectId::from_uuid(row.get(0)),
        row.get(1),
        lifecycle,
        version,
        row.get::<_, DateTime<Utc>>(4),
        row.get::<_, DateTime<Utc>>(5),
    )
    .map_err(|_| StorageError::InvalidStoredData)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_errors_do_not_echo_driver_or_connection_material() {
        let error = StorageError::Connect;
        assert_eq!(error.to_string(), "mg-todo database connection failed");
        assert!(!format!("{error:?}").contains("postgres://"));
    }
}
