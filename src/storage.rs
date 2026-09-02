use crate::{
    config::{DatabaseUrl, validate_local_database_url},
    domain::{DomainError, Lifecycle, Project, ProjectId, Tag, TagId, Todo, TodoId, Version},
    recurrence::{Frequency, RecurrenceError, Rule},
    reminder::{Channel, DeliveryRecord, Reminder, ReminderError},
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_postgres::{Client, GenericClient, NoTls, Row, error::SqlState};

pub const FOUNDATION_MIGRATION: &str = include_str!("../migrations/0001_project_authority.sql");
pub const TAG_MIGRATION: &str = include_str!("../migrations/0002_tag_authority.sql");
pub const TODO_MIGRATION: &str = include_str!("../migrations/0003_todo_authority.sql");
pub const TODO_TAG_MIGRATION: &str = include_str!("../migrations/0004_todo_tag_authority.sql");
pub const AUTHORITY_REVISION_MIGRATION: &str =
    include_str!("../migrations/0005_authority_revision.sql");
pub const TODO_RELATIONSHIP_MIGRATION: &str =
    include_str!("../migrations/0006_todo_relationship_authority.sql");
pub const TODO_RECURRENCE_MIGRATION: &str =
    include_str!("../migrations/0007_todo_recurrence_authority.sql");
pub const TODO_REMINDER_DELIVERY_MIGRATION: &str =
    include_str!("../migrations/0009_todo_reminder_delivery_authority.sql");
pub const TODO_REMINDER_MIGRATION: &str =
    include_str!("../migrations/0008_todo_reminder_authority.sql");
const LEDGER: &str = "mg_todo_schema_migrations";
const MIGRATION_LOCK: i64 = 73_407_463_646;

#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
    pub checksum: &'static str,
    pub table: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "project_authority",
        sql: FOUNDATION_MIGRATION,
        checksum: "0c821017adbb6c219ad371a7729b7d898a178bd9102279d71524504710ed78c0",
        table: "projects",
    },
    Migration {
        version: 2,
        name: "tag_authority",
        sql: TAG_MIGRATION,
        checksum: "4d94c603ee44bbb7e649011d49bdd5325f98ff4abcffa232eb8e0a7770286126",
        table: "tags",
    },
    Migration {
        version: 3,
        name: "todo_authority",
        sql: TODO_MIGRATION,
        checksum: "0f9e31afdb4b2ae562fb065a48b683e0c554cf124ea4e97296ab6630f20af6f2",
        table: "todos",
    },
    Migration {
        version: 4,
        name: "todo_tag_authority",
        sql: TODO_TAG_MIGRATION,
        checksum: "fffb6336ac6d6c02a39dfcf96333858eab15d947594ac50104dfdae57e358716",
        table: "todo_tags",
    },
    Migration {
        version: 5,
        name: "authority_revision",
        sql: AUTHORITY_REVISION_MIGRATION,
        checksum: "680eaac3aac4b42fe2db5e0e681df0a1b4eb4d5170d7751b1a4f0b84e6f9239d",
        table: "mg_todo_authority_state",
    },
    Migration {
        version: 6,
        name: "todo_relationship_authority",
        sql: TODO_RELATIONSHIP_MIGRATION,
        checksum: "9eca75aa95299df898d1bc7e1a0d27fa54de43b04d94e8e7ed0eda0809ecf36d",
        table: "todo_parents",
    },
    Migration {
        version: 7,
        name: "todo_recurrence_authority",
        sql: TODO_RECURRENCE_MIGRATION,
        checksum: "391adbe0240486d80938eb2651d15dd43736d3e9249608a32701ea82e8d20781",
        table: "todo_recurrence",
    },
    Migration {
        version: 8,
        name: "todo_reminder_authority",
        sql: TODO_REMINDER_MIGRATION,
        checksum: "ae7dcac9333555742d19bb5bdc6dc92810769ef394215181c5577655c5f40b38",
        table: "todo_reminders",
    },
    Migration {
        version: 9,
        name: "todo_reminder_delivery_authority",
        sql: TODO_REMINDER_DELIVERY_MIGRATION,
        checksum: "8ce0adab4da891d8e0f152f9907b9db680f32a1b1ae604b57dde173baf31a67f",
        table: "todo_reminder_deliveries",
    },
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationState {
    pub version: i64,
    pub name: String,
    pub applied: bool,
}

/// One repeatable-read view of all currently persisted todo authority state.
#[derive(Debug, Clone)]
pub struct AuthorityExport {
    pub projects: Vec<Project>,
    pub tags: Vec<Tag>,
    pub todos: Vec<Todo>,
    pub revision: u64,
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
    #[error("migration {version} SQL checksum drift")]
    MigrationChecksumDrift { version: i64 },
    #[error("migration {version} SQL checksum is missing")]
    MigrationChecksumMissing { version: i64 },
    #[error("migration {version} schema drift for table '{table}'")]
    MigrationSchemaDrift { version: i64, table: &'static str },
    #[error("unledgered migration table '{table}' already exists")]
    UnledgeredMigrationTable { table: &'static str },
    #[error(
        "migration history gap: migration {applied_version} is applied before migration {missing_version}"
    )]
    MigrationHistoryGap {
        missing_version: i64,
        applied_version: i64,
    },
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
    #[error("tag {tag_id} already exists")]
    TagAlreadyExists { tag_id: TagId },
    #[error("tag {tag_id} was not found")]
    TagNotFound { tag_id: TagId },
    #[error("tag {tag_id} version conflict: expected {expected}, actual {actual}")]
    TagVersionConflict {
        tag_id: TagId,
        expected: u64,
        actual: u64,
    },
    #[error("invalid tag replacement: {reason}")]
    InvalidTagReplacement { reason: &'static str },
    #[error("invalid tag creation: {reason}")]
    InvalidTagCreation { reason: &'static str },
    #[error("{field} must be exactly representable at PostgreSQL microsecond precision")]
    InvalidTimestampPrecision { field: &'static str },
    #[error("invalid stored tag data")]
    InvalidStoredTagData,
    #[error("todo {todo_id} already exists")]
    TodoAlreadyExists { todo_id: TodoId },
    #[error("todo {todo_id} was not found")]
    TodoNotFound { todo_id: TodoId },
    #[error("todo {todo_id} version conflict: expected {expected}, actual {actual}")]
    TodoVersionConflict {
        todo_id: TodoId,
        expected: u64,
        actual: u64,
    },
    #[error("invalid todo creation: {reason}")]
    InvalidTodoCreation { reason: &'static str },
    #[error("invalid todo replacement: {reason}")]
    InvalidTodoReplacement { reason: &'static str },
    #[error("todo project {project_id} was not found")]
    TodoProjectNotFound { project_id: ProjectId },
    #[error("todo relationship conflict: {reason}")]
    TodoRelationshipConflict { reason: &'static str },
    #[error("authoritative {kind} cannot be represented by the current interop schema")]
    UnrepresentableAuthority { kind: &'static str },
    #[error("invalid stored todo data")]
    InvalidStoredTodoData,
    #[error(transparent)]
    Recurrence(#[from] RecurrenceError),
    #[error(transparent)]
    Reminder(#[from] ReminderError),
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

#[derive(Debug, Clone)]
struct AuthoritySchema {
    name: String,
    quoted: String,
}

impl AuthoritySchema {
    fn table(&self, table: &str) -> String {
        format!("{}.{table}", self.quoted)
    }
}

async fn connect_authority(
    database_url: &DatabaseUrl,
    operation: &'static str,
) -> Result<(Client, AuthoritySchema), StorageError> {
    let client = connect(database_url).await?;
    let row = client
        .query_one(
            "SELECT current_schema(), quote_ident(current_schema())",
            &[],
        )
        .await
        .map_err(|_| StorageError::Database { operation })?;
    let name = row
        .get::<_, Option<String>>(0)
        .ok_or(StorageError::Database { operation })?;
    let quoted = row
        .get::<_, Option<String>>(1)
        .ok_or(StorageError::Database { operation })?;
    let restricted_path = format!("{quoted}, pg_catalog");
    client
        .query_one(
            "SELECT set_config('search_path', $1, false)",
            &[&restricted_path],
        )
        .await
        .map_err(|_| StorageError::Database { operation })?;
    Ok((client, AuthoritySchema { name, quoted }))
}

/// Read migration state without creating or changing the ledger.
pub async fn migration_status(
    database_url: &DatabaseUrl,
) -> Result<Vec<MigrationState>, StorageError> {
    let (client, schema) = connect_authority(database_url, "migration status").await?;
    let exists: bool = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
             WHERE table_schema = $1 AND table_name = $2)",
            &[&schema.name, &LEDGER],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "migration status",
        })?
        .get(0);
    if !exists {
        validate_migration_sources()?;
        verify_migration_schemas(&client, &schema, &[]).await?;
        return Ok(MIGRATIONS
            .iter()
            .map(|migration| MigrationState {
                version: migration.version,
                name: migration.name.to_owned(),
                applied: false,
            })
            .collect());
    }

    let has_checksum: bool = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
             WHERE table_schema = $1 AND table_name = $2 \
             AND column_name = 'checksum')",
            &[&schema.name, &LEDGER],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "migration status",
        })?
        .get(0);
    let ledger = schema.table(LEDGER);
    let query = if has_checksum {
        format!("SELECT version, name, checksum FROM {ledger} ORDER BY version")
    } else {
        format!("SELECT version, name, NULL::text AS checksum FROM {ledger} ORDER BY version")
    };
    let rows = client
        .query(&query, &[])
        .await
        .map_err(|_| StorageError::Database {
            operation: "migration status",
        })?;
    validate_recorded_migrations(&rows, has_checksum)?;
    validate_migration_sources()?;
    verify_migration_schemas(&client, &schema, &rows).await?;
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
    let (mut client, schema) = connect_authority(database_url, "migration").await?;
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

    let ledger = schema.table(LEDGER);
    let ledger_exists: bool = transaction
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
             WHERE table_schema = $1 AND table_name = $2)",
            &[&schema.name, &LEDGER],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "migration",
        })?
        .get(0);
    let had_checksum = if ledger_exists {
        transaction
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
                 WHERE table_schema = $1 AND table_name = $2 AND column_name = 'checksum')",
                &[&schema.name, &LEDGER],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "migration",
            })?
            .get(0)
    } else {
        false
    };

    let recorded = if ledger_exists {
        let query = if had_checksum {
            format!("SELECT version, name, checksum FROM {ledger} ORDER BY version")
        } else {
            format!("SELECT version, name, NULL::text AS checksum FROM {ledger} ORDER BY version")
        };
        transaction
            .query(&query, &[])
            .await
            .map_err(|_| StorageError::Database {
                operation: "migration",
            })?
    } else {
        Vec::new()
    };
    validate_recorded_migrations(&recorded, had_checksum)?;
    validate_migration_sources()?;
    verify_migration_schemas(&transaction, &schema, &recorded).await?;

    if !ledger_exists {
        transaction
            .batch_execute(&format!(
                "CREATE TABLE {ledger} (\
                 version bigint PRIMARY KEY, \
                 name text NOT NULL, \
                 checksum text NOT NULL, \
                 applied_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP)"
            ))
            .await
            .map_err(|_| StorageError::Database {
                operation: "migration",
            })?;
    } else if !had_checksum {
        transaction
            .batch_execute(&format!("ALTER TABLE {ledger} ADD COLUMN checksum text"))
            .await
            .map_err(|_| StorageError::Database {
                operation: "migration",
            })?;
        for migration in MIGRATIONS {
            let checksum = migration_checksum(migration);
            transaction
                .execute(
                    &format!(
                        "UPDATE {ledger} SET checksum = $1 \
                         WHERE version = $2 AND name = $3 AND checksum IS NULL"
                    ),
                    &[&checksum, &migration.version, &migration.name],
                )
                .await
                .map_err(|_| StorageError::Database {
                    operation: "migration",
                })?;
        }
        transaction
            .batch_execute(&format!(
                "ALTER TABLE {ledger} ALTER COLUMN checksum SET NOT NULL"
            ))
            .await
            .map_err(|_| StorageError::Database {
                operation: "migration",
            })?;
    }

    for migration in MIGRATIONS {
        let existing = recorded
            .iter()
            .any(|row| row.get::<_, i64>(0) == migration.version);
        if existing {
            continue;
        }
        transaction
            .batch_execute(&qualified_migration_sql(migration, &schema))
            .await
            .map_err(|_| StorageError::Database {
                operation: "migration",
            })?;
        verify_table_schema(&transaction, &schema, migration).await?;
        let checksum = migration_checksum(migration);
        transaction
            .execute(
                &format!("INSERT INTO {ledger} (version, name, checksum) VALUES ($1, $2, $3)"),
                &[&migration.version, &migration.name, &checksum],
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

fn validate_recorded_migrations(rows: &[Row], checksum_required: bool) -> Result<(), StorageError> {
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
        match row.get::<_, Option<String>>(2) {
            Some(checksum) if checksum != migration_checksum(expected) => {
                return Err(StorageError::MigrationChecksumDrift { version });
            }
            None if checksum_required => {
                return Err(StorageError::MigrationChecksumMissing { version });
            }
            Some(_) | None => {}
        }
    }
    for (index, row) in rows.iter().enumerate() {
        let expected = &MIGRATIONS[index];
        let applied_version = row.get::<_, i64>(0);
        if applied_version != expected.version {
            return Err(StorageError::MigrationHistoryGap {
                missing_version: expected.version,
                applied_version,
            });
        }
    }
    Ok(())
}

async fn verify_migration_schemas<C: GenericClient + Sync>(
    client: &C,
    schema: &AuthoritySchema,
    recorded: &[Row],
) -> Result<(), StorageError> {
    for migration in MIGRATIONS {
        let applied = recorded
            .iter()
            .any(|row| row.get::<_, i64>(0) == migration.version);
        let exists: bool = client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
                 WHERE table_schema = $1 AND table_name = $2 AND table_type = 'BASE TABLE')",
                &[&schema.name, &migration.table],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "migration schema verification",
            })?
            .get(0);
        match (applied, exists) {
            (true, true) => verify_table_schema(client, schema, migration).await?,
            (true, false) => {
                return Err(StorageError::MigrationSchemaDrift {
                    version: migration.version,
                    table: migration.table,
                });
            }
            (false, true) => {
                return Err(StorageError::UnledgeredMigrationTable {
                    table: migration.table,
                });
            }
            (false, false) => {}
        }
    }
    Ok(())
}

async fn verify_table_schema<C: GenericClient + Sync>(
    client: &C,
    schema: &AuthoritySchema,
    migration: &Migration,
) -> Result<(), StorageError> {
    let rows = client
        .query(
            "SELECT column_name, udt_name, is_nullable, column_default \
             FROM information_schema.columns \
             WHERE table_schema = $1 AND table_name = $2 ORDER BY ordinal_position",
            &[&schema.name, &migration.table],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "migration schema verification",
        })?;
    let actual = rows
        .iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2),
                row.get::<_, Option<String>>(3),
            )
        })
        .collect::<Vec<_>>();
    let expected = match migration.version {
        1 => vec![
            ("id", "uuid", "NO", None),
            ("name", "text", "NO", None),
            ("lifecycle", "text", "NO", None),
            ("version", "int8", "NO", None),
            ("created_at", "timestamptz", "NO", None),
            ("updated_at", "timestamptz", "NO", None),
        ],
        2 => vec![
            ("id", "uuid", "NO", None),
            ("name", "text", "NO", None),
            ("version", "int8", "NO", None),
            ("created_at", "timestamptz", "NO", None),
            ("updated_at", "timestamptz", "NO", None),
        ],
        3 => vec![
            ("id", "uuid", "NO", None),
            ("title", "text", "NO", None),
            ("project_id", "uuid", "YES", None),
            ("lifecycle", "text", "NO", None),
            ("version", "int8", "NO", None),
            ("created_at", "timestamptz", "NO", None),
            ("updated_at", "timestamptz", "NO", None),
        ],
        4 => vec![
            ("todo_id", "uuid", "NO", None),
            ("tag_id", "uuid", "NO", None),
        ],
        5 => vec![
            ("singleton", "bool", "NO", Some("true")),
            ("revision", "int8", "NO", None),
            ("changed_at", "timestamptz", "NO", None),
        ],
        6 => vec![
            ("child_id", "uuid", "NO", None),
            ("parent_id", "uuid", "NO", None),
        ],
        7 => vec![
            ("todo_id", "uuid", "NO", None),
            ("start_date", "date", "NO", None),
            ("frequency", "text", "NO", None),
            ("interval", "int8", "NO", None),
            ("occurrence_count", "int8", "YES", None),
            ("until_date", "date", "YES", None),
        ],
        8 => vec![
            ("id", "uuid", "NO", None),
            ("todo_id", "uuid", "NO", None),
            ("remind_at", "timestamptz", "NO", None),
            ("channel", "text", "NO", None),
            ("lifecycle", "text", "NO", None),
            ("version", "int8", "NO", None),
            ("created_at", "timestamptz", "NO", None),
            ("updated_at", "timestamptz", "NO", None),
        ],
        9 => vec![
            ("id", "uuid", "NO", None),
            ("reminder_id", "uuid", "NO", None),
            ("idempotency_key", "text", "NO", None),
            ("status", "text", "NO", None),
            ("attempted_at", "timestamptz", "YES", None),
            ("provider_reference", "text", "YES", None),
            ("failure_code", "text", "YES", None),
            ("created_at", "timestamptz", "NO", None),
        ],
        _ => unreachable!("known migrations only"),
    };
    if actual
        != expected
            .into_iter()
            .map(|(name, kind, nullable, default)| {
                (
                    name.to_owned(),
                    kind.to_owned(),
                    nullable.to_owned(),
                    default.map(str::to_owned),
                )
            })
            .collect::<Vec<_>>()
    {
        return Err(StorageError::MigrationSchemaDrift {
            version: migration.version,
            table: migration.table,
        });
    }

    let constraint_rows = client
        .query(
            "SELECT pg_catalog.pg_get_constraintdef(c.oid, true), c.convalidated \
             FROM pg_catalog.pg_constraint c \
             JOIN pg_catalog.pg_class t ON t.oid = c.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             WHERE n.nspname = $1 AND t.relname = $2 AND c.contype IN ('p', 'c', 'f', 'u') \\
             ORDER BY c.contype, c.conname",
            &[&schema.name, &migration.table],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "migration schema verification",
        })?;
    if constraint_rows.iter().any(|row| !row.get::<_, bool>(1)) {
        return Err(StorageError::MigrationSchemaDrift {
            version: migration.version,
            table: migration.table,
        });
    }
    let constraints = constraint_rows
        .iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    let required = match migration.version {
        1 => vec![
            "PRIMARY KEY (id)",
            "CHECK (btrim(name) <> ''::text)",
            "CHECK (lifecycle = ANY (ARRAY['open'::text, 'completed'::text, 'trashed'::text]))",
            "CHECK (version >= 1)",
            "CHECK (updated_at >= created_at)",
        ],
        2 => vec![
            "PRIMARY KEY (id)",
            "CHECK (btrim(name) <> ''::text)",
            "CHECK (version >= 1)",
            "CHECK (updated_at >= created_at)",
        ],
        3 => vec![
            "PRIMARY KEY (id)",
            "FOREIGN KEY (project_id) REFERENCES projects(id)",
            "CHECK (btrim(title) <> ''::text)",
            "CHECK (lifecycle = ANY (ARRAY['open'::text, 'completed'::text, 'trashed'::text]))",
            "CHECK (version >= 1)",
            "CHECK (updated_at >= created_at)",
        ],
        4 => vec![
            "PRIMARY KEY (todo_id, tag_id)",
            "FOREIGN KEY (todo_id) REFERENCES todos(id) ON DELETE CASCADE",
            "FOREIGN KEY (tag_id) REFERENCES tags(id) ON DELETE RESTRICT",
        ],
        5 => vec![
            "PRIMARY KEY (singleton)",
            "CHECK (singleton)",
            "CHECK (revision >= 1)",
        ],
        6 => vec![
            "PRIMARY KEY (child_id)",
            "FOREIGN KEY (child_id) REFERENCES todos(id) ON DELETE CASCADE",
            "FOREIGN KEY (parent_id) REFERENCES todos(id) ON DELETE RESTRICT",
            "CHECK (child_id <> parent_id)",
        ],
        7 => vec![
            "PRIMARY KEY (todo_id)",
            "FOREIGN KEY (todo_id) REFERENCES todos(id) ON DELETE CASCADE",
            "CHECK (frequency = ANY (ARRAY['DAILY'::text, 'WEEKLY'::text, 'MONTHLY'::text]))",
            "CHECK (interval >= 1 AND interval <= 366)",
            "CHECK (occurrence_count >= 1 AND occurrence_count <= 1000 OR occurrence_count IS NULL)",
            "CHECK (occurrence_count IS NOT NULL OR until_date IS NOT NULL)",
            "CHECK (until_date IS NULL OR until_date > start_date)",
        ],
        8 => vec![
            "PRIMARY KEY (id)",
            "FOREIGN KEY (todo_id) REFERENCES todos(id) ON DELETE CASCADE",
            "CHECK (channel = ANY (ARRAY['TUI'::text, 'DESKTOP'::text, 'WEBHOOK'::text]))",
            "CHECK (lifecycle = ANY (ARRAY['active'::text, 'paused'::text, 'cancelled'::text]))",
            "CHECK (version >= 1)",
            "CHECK (updated_at >= created_at)",
        ],
        9 => vec![
            "PRIMARY KEY (id)",
            "FOREIGN KEY (reminder_id) REFERENCES todo_reminders(id) ON DELETE CASCADE",
            "UNIQUE (idempotency_key)",
            "CHECK (btrim(idempotency_key) <> ''::text)",
            "CHECK (attempted_at IS NULL OR attempted_at >= created_at)",
            "CHECK (status = ANY (ARRAY['pending'::text, 'sent'::text, 'failed'::text]))",
            "CHECK (status <> 'sent'::text OR provider_reference IS NOT NULL)",
            "CHECK (status <> 'failed'::text OR failure_code IS NOT NULL)",
            "CHECK (status = 'pending'::text OR attempted_at IS NOT NULL)",
            "CHECK (status <> 'pending'::text OR attempted_at IS NULL)",
        ],
        _ => unreachable!("known migrations only"),
    };
    let mut constraints = constraints;
    constraints.sort_unstable();
    let mut required = required;
    required.sort_unstable();
    if constraints != required {
        return Err(StorageError::MigrationSchemaDrift {
            version: migration.version,
            table: migration.table,
        });
    }
    Ok(())
}

fn migration_checksum(migration: &Migration) -> String {
    migration.checksum.to_owned()
}

fn validate_migration_sources() -> Result<(), StorageError> {
    for migration in MIGRATIONS {
        if calculated_migration_checksum(migration) != migration.checksum {
            return Err(StorageError::MigrationChecksumDrift {
                version: migration.version,
            });
        }
    }
    Ok(())
}

fn calculated_migration_checksum(migration: &Migration) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    Sha256::digest(migration.sql.as_bytes())
        .iter()
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 0x0f)]),
            ]
        })
        .collect()
}

fn qualified_migration_sql(migration: &Migration, schema: &AuthoritySchema) -> String {
    migration.sql.replacen(
        "CREATE TABLE ",
        &format!("CREATE TABLE {}.", schema.quoted),
        1,
    )
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
        validate_timestamp_precision(project.created_at(), "created_at")?;
        validate_timestamp_precision(project.updated_at(), "updated_at")?;
        let version = database_version(project.version())?;
        let (client, schema) = connect_authority(&self.database_url, "project create").await?;
        let projects = schema.table("projects");
        let lifecycle = lifecycle_text(project.lifecycle());
        let result = client
            .execute(
                &format!(
                    "INSERT INTO {projects} \
                     (id, name, lifecycle, version, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, $5, $6)"
                ),
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
        let (client, schema) = connect_authority(&self.database_url, "project find").await?;
        let projects = schema.table("projects");
        client
            .query_opt(
                &format!(
                    "SELECT id, name, lifecycle, version, created_at, updated_at \
                 FROM {projects} WHERE id = $1"
                ),
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
        let (client, schema) = connect_authority(&self.database_url, "project list").await?;
        let projects = schema.table("projects");
        let rows = client
            .query(
                &format!(
                    "SELECT id, name, lifecycle, version, created_at, updated_at \
                 FROM {projects} ORDER BY id"
                ),
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
        let (mut client, schema) = connect_authority(&self.database_url, "project replace").await?;
        let projects = schema.table("projects");
        let transaction = client
            .transaction()
            .await
            .map_err(|_| StorageError::Database {
                operation: "project replace",
            })?;
        let row = transaction
            .query_opt(
                &format!(
                    "SELECT id, name, lifecycle, version, created_at, updated_at \
                 FROM {projects} WHERE id = $1 FOR UPDATE"
                ),
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
        validate_timestamp_precision(replacement.created_at(), "created_at")?;
        validate_timestamp_precision(replacement.updated_at(), "updated_at")?;
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
                &format!("UPDATE {projects} SET name = $1, lifecycle = $2, version = $3, updated_at = $4 \
                 WHERE id = $5 AND version = $6"),
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

#[derive(Debug, Clone)]
pub struct PostgresTagRepository {
    database_url: DatabaseUrl,
}

impl PostgresTagRepository {
    #[must_use]
    pub const fn new(database_url: DatabaseUrl) -> Self {
        Self { database_url }
    }

    /// Insert a validated tag without rewriting caller-owned identity or history.
    pub async fn create(&self, tag: &Tag) -> Result<(), StorageError> {
        revalidate_tag(tag)?;
        validate_timestamp_precision(tag.created_at(), "created_at")?;
        validate_timestamp_precision(tag.updated_at(), "updated_at")?;
        let version = tag_creation_database_version(tag.version())?;
        let (client, schema) = connect_authority(&self.database_url, "tag create").await?;
        let tags = schema.table("tags");
        let result = client
            .execute(
                &format!(
                    "INSERT INTO {tags} (id, name, version, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5)"
                ),
                &[
                    &tag.id().as_uuid(),
                    &tag.name(),
                    &version,
                    &tag.created_at(),
                    &tag.updated_at(),
                ],
            )
            .await;
        match result {
            Ok(1) => Ok(()),
            Ok(_) => Err(StorageError::Database {
                operation: "tag create",
            }),
            Err(error) if error.code() == Some(&SqlState::UNIQUE_VIOLATION) => {
                Err(StorageError::TagAlreadyExists { tag_id: tag.id() })
            }
            Err(_) => Err(StorageError::Database {
                operation: "tag create",
            }),
        }
    }

    pub async fn find(&self, tag_id: TagId) -> Result<Option<Tag>, StorageError> {
        let (client, schema) = connect_authority(&self.database_url, "tag find").await?;
        let tags = schema.table("tags");
        client
            .query_opt(
                &format!(
                    "SELECT id, name, version, created_at, updated_at FROM {tags} WHERE id = $1"
                ),
                &[&tag_id.as_uuid()],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "tag find",
            })?
            .as_ref()
            .map(tag_from_row)
            .transpose()
    }

    /// List every tag in deterministic ID order without changing authority.
    pub async fn list(&self) -> Result<Vec<Tag>, StorageError> {
        let (client, schema) = connect_authority(&self.database_url, "tag list").await?;
        let tags = schema.table("tags");
        let rows = client
            .query(
                &format!(
                    "SELECT id, name, version, created_at, updated_at FROM {tags} ORDER BY id"
                ),
                &[],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "tag list",
            })?;
        rows.iter().map(tag_from_row).collect()
    }

    /// Replace one tag under a row lock and caller-supplied optimistic version.
    pub async fn replace(&self, expected: Version, replacement: &Tag) -> Result<(), StorageError> {
        revalidate_tag(replacement)?;
        let (mut client, schema) = connect_authority(&self.database_url, "tag replace").await?;
        let tags = schema.table("tags");
        let transaction = client
            .transaction()
            .await
            .map_err(|_| StorageError::Database {
                operation: "tag replace",
            })?;
        let row = transaction
            .query_opt(
                &format!(
                    "SELECT id, name, version, created_at, updated_at \
                 FROM {tags} WHERE id = $1 FOR UPDATE"
                ),
                &[&replacement.id().as_uuid()],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "tag replace",
            })?
            .ok_or(StorageError::TagNotFound {
                tag_id: replacement.id(),
            })?;
        let current = tag_from_row(&row)?;
        if current.version() != expected {
            return Err(StorageError::TagVersionConflict {
                tag_id: replacement.id(),
                expected: expected.value(),
                actual: current.version().value(),
            });
        }
        validate_timestamp_precision(replacement.created_at(), "created_at")?;
        validate_timestamp_precision(replacement.updated_at(), "updated_at")?;
        if replacement.version() != expected.next()? {
            return Err(StorageError::InvalidTagReplacement {
                reason: "replacement version must be expected version plus one",
            });
        }
        if replacement.created_at() != current.created_at() {
            return Err(StorageError::InvalidTagReplacement {
                reason: "created_at is immutable",
            });
        }
        if replacement.updated_at() < current.updated_at() {
            return Err(StorageError::InvalidTagReplacement {
                reason: "updated_at must not move backward",
            });
        }
        let changed = transaction
            .execute(
                &format!(
                    "UPDATE {tags} SET name = $1, version = $2, updated_at = $3 \
                 WHERE id = $4 AND version = $5"
                ),
                &[
                    &replacement.name(),
                    &tag_database_version(replacement.version())?,
                    &replacement.updated_at(),
                    &replacement.id().as_uuid(),
                    &tag_database_version(expected)?,
                ],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "tag replace",
            })?;
        if changed != 1 {
            return Err(StorageError::Database {
                operation: "tag replace",
            });
        }
        transaction
            .commit()
            .await
            .map_err(|_| StorageError::Database {
                operation: "tag replace",
            })
    }
}

/// PostgreSQL authority for core todo state. Relationship, recurrence, and reminder
/// fields are rejected until their own append-only migrations define those contracts.
#[derive(Debug, Clone)]
pub struct PostgresTodoRepository {
    database_url: DatabaseUrl,
}

impl PostgresTodoRepository {
    #[must_use]
    pub const fn new(database_url: DatabaseUrl) -> Self {
        Self { database_url }
    }

    pub async fn create(&self, todo: &Todo) -> Result<(), StorageError> {
        revalidate_todo(todo)?;

        validate_timestamp_precision(todo.created_at(), "created_at")?;
        validate_timestamp_precision(todo.updated_at(), "updated_at")?;
        let version = todo_creation_database_version(todo.version())?;
        let (mut client, schema) = connect_authority(&self.database_url, "todo create").await?;
        let transaction = client
            .transaction()
            .await
            .map_err(|_| StorageError::Database {
                operation: "todo create",
            })?;
        validate_todo_project(&transaction, &schema, todo.project_id()).await?;
        validate_todo_relationships(&transaction, &schema, todo).await?;
        let todos = schema.table("todos");
        let lifecycle = lifecycle_text(todo.lifecycle());
        let result = transaction
            .execute(
                &format!(
                    "INSERT INTO {todos} \
                     (id, title, project_id, lifecycle, version, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7)"
                ),
                &[
                    &todo.id().as_uuid(),
                    &todo.title(),
                    &todo.project_id().map(ProjectId::as_uuid),
                    &lifecycle,
                    &version,
                    &todo.created_at(),
                    &todo.updated_at(),
                ],
            )
            .await;
        match result {
            Ok(1) => {
                persist_todo_relationships(&transaction, &schema, todo).await?;
                transaction
                    .commit()
                    .await
                    .map_err(|_| StorageError::Database {
                        operation: "todo create",
                    })
            }
            Ok(_) => Err(StorageError::Database {
                operation: "todo create",
            }),
            Err(error) if error.code() == Some(&SqlState::UNIQUE_VIOLATION) => {
                Err(StorageError::TodoAlreadyExists { todo_id: todo.id() })
            }
            Err(_) => Err(StorageError::Database {
                operation: "todo create",
            }),
        }
    }

    pub async fn find(&self, todo_id: TodoId) -> Result<Option<Todo>, StorageError> {
        let (client, schema) = connect_authority(&self.database_url, "todo find").await?;
        let todos = schema.table("todos");
        let row = client
            .query_opt(
                &format!(
                    "SELECT id, title, project_id, lifecycle, version, created_at, updated_at \
                     FROM {todos} WHERE id = $1"
                ),
                &[&todo_id.as_uuid()],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "todo find",
            })?;
        let todo = row.as_ref().map(todo_from_row).transpose()?;
        match todo {
            Some(todo) => load_todo_relationships(&client, &schema, todo)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    /// List core todos in deterministic ID order.
    pub async fn list(&self) -> Result<Vec<Todo>, StorageError> {
        let (client, schema) = connect_authority(&self.database_url, "todo list").await?;
        let todos = schema.table("todos");
        let rows = client
            .query(
                &format!(
                    "SELECT id, title, project_id, lifecycle, version, created_at, updated_at \
                     FROM {todos} ORDER BY id"
                ),
                &[],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "todo list",
            })?;
        let mut result = Vec::with_capacity(rows.len());
        for row in &rows {
            let todo = todo_from_row(row)?;
            result.push(load_todo_relationships(&client, &schema, todo).await?);
        }
        Ok(result)
    }

    pub async fn replace(&self, expected: Version, replacement: &Todo) -> Result<(), StorageError> {
        revalidate_todo(replacement)?;
        validate_todo_foundation(replacement)
            .map_err(|reason| StorageError::InvalidTodoReplacement { reason })?;
        let (mut client, schema) = connect_authority(&self.database_url, "todo replace").await?;
        let transaction = client
            .transaction()
            .await
            .map_err(|_| StorageError::Database {
                operation: "todo replace",
            })?;
        let todos = schema.table("todos");
        let row = transaction
            .query_opt(
                &format!(
                    "SELECT id, title, project_id, lifecycle, version, created_at, updated_at \
                     FROM {todos} WHERE id = $1 FOR UPDATE"
                ),
                &[&replacement.id().as_uuid()],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "todo replace",
            })?
            .ok_or(StorageError::TodoNotFound {
                todo_id: replacement.id(),
            })?;
        let current = load_todo_relationships(&transaction, &schema, todo_from_row(&row)?).await?;
        if current.version() != expected {
            return Err(StorageError::TodoVersionConflict {
                todo_id: replacement.id(),
                expected: expected.value(),
                actual: current.version().value(),
            });
        }
        validate_timestamp_precision(replacement.created_at(), "created_at")?;
        validate_timestamp_precision(replacement.updated_at(), "updated_at")?;
        current.lifecycle().transition(replacement.lifecycle())?;
        if replacement.version() != expected.next()? {
            return Err(StorageError::InvalidTodoReplacement {
                reason: "replacement version must be expected version plus one",
            });
        }
        if replacement.created_at() != current.created_at() {
            return Err(StorageError::InvalidTodoReplacement {
                reason: "created_at is immutable",
            });
        }
        if replacement.updated_at() < current.updated_at() {
            return Err(StorageError::InvalidTodoReplacement {
                reason: "updated_at must not move backward",
            });
        }
        validate_todo_project(&transaction, &schema, replacement.project_id()).await?;
        validate_todo_relationships(&transaction, &schema, replacement).await?;
        let lifecycle = lifecycle_text(replacement.lifecycle());
        let changed = transaction
            .execute(
                &format!(
                    "UPDATE {todos} SET title = $1, project_id = $2, lifecycle = $3, \
                     version = $4, updated_at = $5 WHERE id = $6 AND version = $7"
                ),
                &[
                    &replacement.title(),
                    &replacement.project_id().map(ProjectId::as_uuid),
                    &lifecycle,
                    &todo_database_version(replacement.version())?,
                    &replacement.updated_at(),
                    &replacement.id().as_uuid(),
                    &todo_database_version(expected)?,
                ],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "todo replace",
            })?;
        if changed != 1 {
            return Err(StorageError::Database {
                operation: "todo replace",
            });
        }
        persist_todo_relationships(&transaction, &schema, replacement).await?;
        transaction
            .commit()
            .await
            .map_err(|_| StorageError::Database {
                operation: "todo replace",
            })
    }
}

async fn validate_todo_relationships<C: GenericClient + Sync>(
    client: &C,
    schema: &AuthoritySchema,
    todo: &Todo,
) -> Result<(), StorageError> {
    let todos = schema.table("todos");
    let tags = schema.table("tags");
    let parents = schema.table("todo_parents");
    let dependencies = schema.table("todo_dependencies");
    if let Some(parent) = todo.parent_id() {
        if parent == todo.id() {
            return Err(StorageError::TodoRelationshipConflict {
                reason: "todo cannot be its own parent",
            });
        }
        let exists: bool = client
            .query_one(
                &format!("SELECT EXISTS (SELECT 1 FROM {todos} WHERE id = $1)"),
                &[&parent.as_uuid()],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "todo parent validation",
            })?
            .get(0);
        if !exists {
            return Err(StorageError::TodoRelationshipConflict {
                reason: "parent todo was not found",
            });
        }
        let cycle: bool = client.query_one(&format!("WITH RECURSIVE ancestors(id) AS (SELECT $1::uuid UNION SELECT p.parent_id FROM {parents} p JOIN ancestors a ON p.child_id = a.id) SELECT EXISTS (SELECT 1 FROM ancestors WHERE id = $2)"), &[&parent.as_uuid(), &todo.id().as_uuid()]).await
            .map_err(|_| StorageError::Database { operation: "todo parent validation" })?.get(0);
        if cycle {
            return Err(StorageError::TodoRelationshipConflict {
                reason: "parent relationship would create a cycle",
            });
        }
    }
    for dependency in todo.dependency_ids() {
        if *dependency == todo.id() {
            return Err(StorageError::TodoRelationshipConflict {
                reason: "todo cannot depend on itself",
            });
        }
        let exists: bool = client
            .query_one(
                &format!("SELECT EXISTS (SELECT 1 FROM {todos} WHERE id = $1)"),
                &[&dependency.as_uuid()],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "todo dependency validation",
            })?
            .get(0);
        if !exists {
            return Err(StorageError::TodoRelationshipConflict {
                reason: "dependency todo was not found",
            });
        }
        let cycle: bool = client.query_one(&format!("WITH RECURSIVE reach(id) AS (SELECT $1::uuid UNION SELECT d.prerequisite_id FROM {dependencies} d JOIN reach r ON d.dependent_id = r.id) SELECT EXISTS (SELECT 1 FROM reach WHERE id = $2)"), &[&dependency.as_uuid(), &todo.id().as_uuid()]).await
            .map_err(|_| StorageError::Database { operation: "todo dependency validation" })?.get(0);
        if cycle {
            return Err(StorageError::TodoRelationshipConflict {
                reason: "dependency relationship would create a cycle",
            });
        }
    }
    for tag in todo.tag_ids() {
        let exists: bool = client
            .query_one(
                &format!("SELECT EXISTS (SELECT 1 FROM {tags} WHERE id = $1)"),
                &[&tag.as_uuid()],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "todo tag validation",
            })?
            .get(0);
        if !exists {
            return Err(StorageError::TodoRelationshipConflict {
                reason: "tag was not found",
            });
        }
    }
    Ok(())
}

async fn persist_todo_relationships<C: GenericClient + Sync>(
    client: &C,
    schema: &AuthoritySchema,
    todo: &Todo,
) -> Result<(), StorageError> {
    let todo_id = todo.id().as_uuid();
    let parents = schema.table("todo_parents");
    let dependencies = schema.table("todo_dependencies");
    let tags = schema.table("todo_tags");
    client
        .execute(
            &format!("DELETE FROM {parents} WHERE child_id = $1"),
            &[&todo_id],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "todo relationship write",
        })?;
    client
        .execute(
            &format!("DELETE FROM {dependencies} WHERE dependent_id = $1"),
            &[&todo_id],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "todo relationship write",
        })?;
    client
        .execute(
            &format!("DELETE FROM {tags} WHERE todo_id = $1"),
            &[&todo_id],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "todo relationship write",
        })?;
    if let Some(parent) = todo.parent_id() {
        client
            .execute(
                &format!("INSERT INTO {parents} (child_id, parent_id) VALUES ($1, $2)"),
                &[&todo_id, &parent.as_uuid()],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "todo relationship write",
            })?;
    }
    for dependency in todo.dependency_ids() {
        client
            .execute(
                &format!(
                    "INSERT INTO {dependencies} (dependent_id, prerequisite_id) VALUES ($1, $2)"
                ),
                &[&todo_id, &dependency.as_uuid()],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "todo relationship write",
            })?;
    }
    for tag in todo.tag_ids() {
        client
            .execute(
                &format!("INSERT INTO {tags} (todo_id, tag_id) VALUES ($1, $2)"),
                &[&todo_id, &tag.as_uuid()],
            )
            .await
            .map_err(|_| StorageError::Database {
                operation: "todo relationship write",
            })?;
    }
    Ok(())
}

async fn load_todo_relationships<C: GenericClient + Sync>(
    client: &C,
    schema: &AuthoritySchema,
    todo: Todo,
) -> Result<Todo, StorageError> {
    let id = todo.id().as_uuid();
    let parent_table = schema.table("todo_parents");
    let dependency_table = schema.table("todo_dependencies");
    let tag_table = schema.table("todo_tags");
    let parent = client
        .query_opt(
            &format!("SELECT parent_id FROM {parent_table} WHERE child_id = $1"),
            &[&id],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "todo relationship read",
        })?
        .map(|row| TodoId::from_uuid(row.get(0)));
    let dependencies = client.query(
        &format!("SELECT prerequisite_id FROM {dependency_table} WHERE dependent_id = $1 ORDER BY prerequisite_id"), &[&id]
    ).await.map_err(|_| StorageError::Database { operation: "todo relationship read" })?
        .into_iter().map(|row| TodoId::from_uuid(row.get(0))).collect();
    let tags = client
        .query(
            &format!("SELECT tag_id FROM {tag_table} WHERE todo_id = $1 ORDER BY tag_id"),
            &[&id],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "todo relationship read",
        })?
        .into_iter()
        .map(|row| TagId::from_uuid(row.get(0)))
        .collect();
    Todo::new(
        todo.id(),
        todo.title().to_owned(),
        todo.project_id(),
        parent,
        tags,
        dependencies,
        todo.lifecycle(),
        todo.version(),
        todo.created_at(),
        todo.updated_at(),
    )
    .map_err(StorageError::Domain)
}

async fn validate_todo_project<C: GenericClient + Sync>(
    client: &C,
    schema: &AuthoritySchema,
    project_id: Option<ProjectId>,
) -> Result<(), StorageError> {
    let Some(project_id) = project_id else {
        return Ok(());
    };
    let projects = schema.table("projects");
    let exists: bool = client
        .query_one(
            &format!("SELECT EXISTS (SELECT 1 FROM {projects} WHERE id = $1)"),
            &[&project_id.as_uuid()],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "todo project validation",
        })?
        .get(0);
    if exists {
        Ok(())
    } else {
        Err(StorageError::TodoProjectNotFound { project_id })
    }
}

fn validate_todo_foundation(todo: &Todo) -> Result<(), &'static str> {
    if todo.parent_id().is_some() {
        return Err("parent relationships are not enabled yet");
    }
    if !todo.tag_ids().is_empty() {
        return Err("tag relationships are not enabled yet");
    }
    if !todo.dependency_ids().is_empty() {
        return Err("dependency relationships are not enabled yet");
    }
    Ok(())
}

fn revalidate_todo(todo: &Todo) -> Result<(), StorageError> {
    Todo::new(
        todo.id(),
        todo.title().to_owned(),
        todo.project_id(),
        todo.parent_id(),
        todo.tag_ids().to_vec(),
        todo.dependency_ids().to_vec(),
        todo.lifecycle(),
        todo.version(),
        todo.created_at(),
        todo.updated_at(),
    )?;
    Ok(())
}

fn todo_creation_database_version(version: Version) -> Result<i64, StorageError> {
    i64::try_from(version.value()).map_err(|_| StorageError::InvalidTodoCreation {
        reason: "version exceeds PostgreSQL bigint range",
    })
}

fn todo_database_version(version: Version) -> Result<i64, StorageError> {
    i64::try_from(version.value()).map_err(|_| StorageError::InvalidTodoReplacement {
        reason: "version exceeds PostgreSQL bigint range",
    })
}

fn todo_from_row(row: &Row) -> Result<Todo, StorageError> {
    let lifecycle = match row.get::<_, &str>(3) {
        "open" => Lifecycle::Open,
        "completed" => Lifecycle::Completed,
        "trashed" => Lifecycle::Trashed,
        _ => return Err(StorageError::InvalidStoredTodoData),
    };
    let raw_version = row.get::<_, i64>(4);
    let version = u64::try_from(raw_version)
        .ok()
        .and_then(|value| Version::try_from_value(value).ok())
        .ok_or(StorageError::InvalidStoredTodoData)?;
    Todo::new(
        TodoId::from_uuid(row.get(0)),
        row.get(1),
        row.get::<_, Option<uuid::Uuid>>(2)
            .map(ProjectId::from_uuid),
        None,
        vec![],
        vec![],
        lifecycle,
        version,
        row.get::<_, DateTime<Utc>>(5),
        row.get::<_, DateTime<Utc>>(6),
    )
    .map_err(|_| StorageError::InvalidStoredTodoData)
}

fn revalidate_tag(tag: &Tag) -> Result<(), StorageError> {
    Tag::new(
        tag.id(),
        tag.name().to_owned(),
        tag.version(),
        tag.created_at(),
        tag.updated_at(),
    )?;
    Ok(())
}

fn tag_creation_database_version(version: Version) -> Result<i64, StorageError> {
    i64::try_from(version.value()).map_err(|_| StorageError::InvalidTagCreation {
        reason: "version exceeds PostgreSQL bigint range",
    })
}

fn tag_database_version(version: Version) -> Result<i64, StorageError> {
    i64::try_from(version.value()).map_err(|_| StorageError::InvalidTagReplacement {
        reason: "version exceeds PostgreSQL bigint range",
    })
}

fn validate_timestamp_precision(
    timestamp: DateTime<Utc>,
    field: &'static str,
) -> Result<(), StorageError> {
    if timestamp.timestamp_subsec_nanos() % 1_000 == 0 {
        Ok(())
    } else {
        Err(StorageError::InvalidTimestampPrecision { field })
    }
}

fn tag_from_row(row: &Row) -> Result<Tag, StorageError> {
    let raw_version = row.get::<_, i64>(2);
    let version = u64::try_from(raw_version)
        .ok()
        .and_then(|value| Version::try_from_value(value).ok())
        .ok_or(StorageError::InvalidStoredTagData)?;
    Tag::new(
        TagId::from_uuid(row.get(0)),
        row.get(1),
        version,
        row.get::<_, DateTime<Utc>>(3),
        row.get::<_, DateTime<Utc>>(4),
    )
    .map_err(|_| StorageError::InvalidStoredTagData)
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

/// Read the complete currently representable authority in one transaction.
///
/// Persist a validated reminder and verify the database returned its identity.
pub async fn create_reminder(
    database_url: &DatabaseUrl,
    reminder: &Reminder,
) -> Result<(), StorageError> {
    reminder.validate()?;
    validate_timestamp_precision(reminder.remind_at, "remind_at")?;
    validate_timestamp_precision(reminder.created_at, "created_at")?;
    validate_timestamp_precision(reminder.updated_at, "updated_at")?;
    let (client, schema) = connect_authority(database_url, "reminder create").await?;
    let table = schema.table("todo_reminders");
    let channel = match reminder.channel {
        Channel::Tui => "TUI",
        Channel::Desktop => "DESKTOP",
        Channel::Webhook => "WEBHOOK",
    };
    let lifecycle = match reminder.lifecycle {
        crate::reminder::ReminderLifecycle::Active => "active",
        crate::reminder::ReminderLifecycle::Paused => "paused",
        crate::reminder::ReminderLifecycle::Cancelled => "cancelled",
    };
    let row = client
        .query_one(
            &format!(
                "INSERT INTO {table} (id, todo_id, remind_at, channel, lifecycle, version, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id"
            ),
            &[
                &reminder.id.as_uuid(),
                &reminder.todo_id.as_uuid(),
                &reminder.remind_at,
                &channel,
                &lifecycle,
                &(reminder.version as i64),
                &reminder.created_at,
                &reminder.updated_at,
            ],
        )
        .await
        .map_err(|_| StorageError::Database { operation: "reminder create" })?;
    if row.get::<_, uuid::Uuid>(0) != reminder.id.as_uuid() {
        return Err(StorageError::Database {
            operation: "reminder create",
        });
    }
    Ok(())
}

/// Persist a pending, sent, or failed delivery record and verify its identity.
pub async fn create_delivery_record(
    database_url: &DatabaseUrl,
    record: &DeliveryRecord,
) -> Result<(), StorageError> {
    record.validate()?;
    validate_timestamp_precision(record.created_at, "created_at")?;
    if let Some(attempted_at) = record.attempted_at {
        validate_timestamp_precision(attempted_at, "attempted_at")?;
    }
    let (client, schema) = connect_authority(database_url, "delivery create").await?;
    let table = schema.table("todo_reminder_deliveries");
    let status = match record.status {
        crate::reminder::DeliveryStatus::Pending => "pending",
        crate::reminder::DeliveryStatus::Sent => "sent",
        crate::reminder::DeliveryStatus::Failed => "failed",
    };
    let row = client
        .query_one(
            &format!(
                "INSERT INTO {table} (id, reminder_id, idempotency_key, status, attempted_at, provider_reference, failure_code, created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id"
            ),
            &[
                &record.id.as_uuid(),
                &record.reminder_id.as_uuid(),
                &record.idempotency_key,
                &status,
                &record.attempted_at,
                &record.provider_reference,
                &record.failure_code,
                &record.created_at,
            ],
        )
        .await
        .map_err(|_| StorageError::Database { operation: "delivery create" })?;
    if row.get::<_, uuid::Uuid>(0) != record.id.as_uuid() {
        return Err(StorageError::Database {
            operation: "delivery create",
        });
    }
    Ok(())
}

/// Replace the bounded recurrence rule for one authoritative todo atomically.
pub async fn set_todo_recurrence(
    database_url: &DatabaseUrl,
    todo_id: TodoId,
    start: NaiveDate,
    rule: &Rule,
) -> Result<(), StorageError> {
    rule.validate(start)?;
    let (mut client, schema) = connect_authority(database_url, "recurrence set").await?;
    let transaction = client
        .transaction()
        .await
        .map_err(|_| StorageError::Database {
            operation: "recurrence set",
        })?;
    let todos = schema.table("todos");
    let exists = transaction
        .query_opt(
            &format!("SELECT 1 FROM {todos} WHERE id = $1"),
            &[&todo_id.as_uuid()],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "recurrence set",
        })?
        .is_some();
    if !exists {
        return Err(StorageError::InvalidStoredTodoData);
    }
    let recurrence = schema.table("todo_recurrence");
    let frequency = match rule.frequency {
        Frequency::Daily => "DAILY",
        Frequency::Weekly => "WEEKLY",
        Frequency::Monthly => "MONTHLY",
    };
    transaction
        .execute(
            &format!(
                "INSERT INTO {recurrence} (todo_id, start_date, frequency, interval, occurrence_count, until_date) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (todo_id) DO UPDATE SET start_date = EXCLUDED.start_date, frequency = EXCLUDED.frequency, \
                 interval = EXCLUDED.interval, occurrence_count = EXCLUDED.occurrence_count, until_date = EXCLUDED.until_date"
            ),
            &[&todo_id.as_uuid(), &start, &frequency, &i64::from(rule.interval), &rule.count.map(i64::from), &rule.until],
        )
        .await
        .map_err(|_| StorageError::Database { operation: "recurrence set" })?;
    transaction
        .commit()
        .await
        .map_err(|_| StorageError::Database {
            operation: "recurrence set",
        })?;
    Ok(())
}

/// Read the authoritative recurrence rule for one todo.
pub async fn find_todo_recurrence(
    database_url: &DatabaseUrl,
    todo_id: TodoId,
) -> Result<Option<(NaiveDate, Rule)>, StorageError> {
    let (client, schema) = connect_authority(database_url, "recurrence find").await?;
    let recurrence = schema.table("todo_recurrence");
    let row = client
        .query_opt(
            &format!("SELECT start_date, frequency, interval, occurrence_count, until_date FROM {recurrence} WHERE todo_id = $1"),
            &[&todo_id.as_uuid()],
        )
        .await
        .map_err(|_| StorageError::Database { operation: "recurrence find" })?;
    let Some(row) = row else { return Ok(None) };
    let start: NaiveDate = row
        .try_get(0)
        .map_err(|_| StorageError::InvalidStoredTodoData)?;
    let frequency = match row
        .try_get::<_, String>(1)
        .map_err(|_| StorageError::InvalidStoredTodoData)?
        .as_str()
    {
        "DAILY" => Frequency::Daily,
        "WEEKLY" => Frequency::Weekly,
        "MONTHLY" => Frequency::Monthly,
        _ => return Err(StorageError::InvalidStoredTodoData),
    };
    let interval = u32::try_from(
        row.try_get::<_, i64>(2)
            .map_err(|_| StorageError::InvalidStoredTodoData)?,
    )
    .map_err(|_| StorageError::InvalidStoredTodoData)?;
    let count = row
        .try_get::<_, Option<i64>>(3)
        .map_err(|_| StorageError::InvalidStoredTodoData)?
        .map(u32::try_from)
        .transpose()
        .map_err(|_| StorageError::InvalidStoredTodoData)?;
    let until = row
        .try_get(4)
        .map_err(|_| StorageError::InvalidStoredTodoData)?;
    let rule = Rule::new(frequency, interval, count, until, start)?;
    Ok(Some((start, rule)))
}

/// Relationship, recurrence, and reminder rows are not fabricated here; each
/// must be added to the export contract with its own migration and tests.
pub async fn export_authority(database_url: &DatabaseUrl) -> Result<AuthorityExport, StorageError> {
    let (mut client, schema) = connect_authority(database_url, "interop export").await?;
    let transaction = client
        .transaction()
        .await
        .map_err(|_| StorageError::Database {
            operation: "interop export",
        })?;
    let projects_table = schema.table("projects");
    let tags_table = schema.table("tags");
    let todos_table = schema.table("todos");
    let project_rows = transaction
        .query(
            &format!("SELECT id, name, lifecycle, version, created_at, updated_at FROM {projects_table} ORDER BY id"),
            &[],
        )
        .await
        .map_err(|_| StorageError::Database { operation: "interop export" })?;
    let tag_rows = transaction
        .query(
            &format!(
                "SELECT id, name, version, created_at, updated_at FROM {tags_table} ORDER BY id"
            ),
            &[],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "interop export",
        })?;
    let todo_rows = transaction
        .query(
            &format!("SELECT id, title, project_id, lifecycle, version, created_at, updated_at FROM {todos_table} ORDER BY id"),
            &[],
        )
        .await
        .map_err(|_| StorageError::Database { operation: "interop export" })?;
    let state_table = schema.table("mg_todo_authority_state");
    let revision = transaction
        .query_opt(
            &format!("SELECT revision FROM {state_table} WHERE singleton = true"),
            &[],
        )
        .await
        .map_err(|_| StorageError::Database {
            operation: "interop export",
        })?
        .and_then(|row| u64::try_from(row.get::<_, i64>(0)).ok())
        .ok_or(StorageError::Database {
            operation: "interop export",
        })?;
    let projects = project_rows
        .iter()
        .map(project_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let tags = tag_rows
        .iter()
        .map(tag_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let mut todos = Vec::with_capacity(todo_rows.len());
    for row in &todo_rows {
        todos.push(load_todo_relationships(&transaction, &schema, todo_from_row(row)?).await?);
    }
    transaction
        .commit()
        .await
        .map_err(|_| StorageError::Database {
            operation: "interop export",
        })?;
    Ok(AuthorityExport {
        projects,
        tags,
        todos,
        revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    #[test]
    fn storage_errors_do_not_echo_driver_or_connection_material() {
        let error = StorageError::Connect;
        assert_eq!(error.to_string(), "mg-todo database connection failed");
        assert!(!format!("{error:?}").contains("postgres://"));
    }

    #[test]
    fn postgres_timestamp_precision_contract_rejects_sub_microseconds() {
        let exact = Utc
            .with_ymd_and_hms(2026, 8, 28, 12, 0, 0)
            .unwrap()
            .with_nanosecond(123_456_000)
            .unwrap();
        assert_eq!(validate_timestamp_precision(exact, "created_at"), Ok(()));

        let inexact = exact + chrono::Duration::nanoseconds(1);
        assert_eq!(
            validate_timestamp_precision(inexact, "created_at"),
            Err(StorageError::InvalidTimestampPrecision {
                field: "created_at"
            })
        );
    }

    #[test]
    fn tag_creation_version_overflow_has_creation_error_contract() {
        let version = Version::try_from_value(u64::MAX).unwrap();
        let error = tag_creation_database_version(version).unwrap_err();
        assert_eq!(
            error,
            StorageError::InvalidTagCreation {
                reason: "version exceeds PostgreSQL bigint range"
            }
        );
        assert_eq!(
            error.to_string(),
            "invalid tag creation: version exceeds PostgreSQL bigint range"
        );
    }
}
