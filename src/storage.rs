// Author: Jeff
// Date: 2026-09-19
// Description: The SQLite authority for projects, tags, todos, and their recorded history
// Notes: One file, WAL journal, foreign keys on, and an append-only migration ledger.
//        Instants are stored as fixed-width RFC 3339 microseconds in UTC and civil dates
//        as YYYY-MM-DD, so SQLite's text comparison is time comparison and the CHECK
//        constraints the schema carries mean what they say. Every call takes its own
//        connection, the way the PostgreSQL layer took its own session; a write that has
//        to read first opens an immediate transaction so the optimistic version check and
//        the write it authorizes cannot be interleaved.

use crate::{
    domain::{
        DomainError, Lifecycle, Project, ProjectId, Tag, TagId, Todo, TodoDue, TodoId, Version,
    },
    recurrence::{Frequency, RecurrenceError, Rule},
    reminder::{
        Channel, DeliveryRecord, DeliveryStatus, Reminder, ReminderError, ReminderLifecycle,
    },
};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;
use uuid::Uuid;

// ── The embedded schema ──

pub const AUTHORITY_MIGRATION: &str = include_str!("../migrations/0001_remindr_authority.sql");

// How long a call waits for another writer before giving up
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const LEDGER: &str = "schema_migrations";
const JOURNAL_MODE_WAL: &str = "wal";
// A civil date is written the way a person writes one
const DATE_FORMAT: &str = "%Y-%m-%d";
// The one stored instant shape: fixed width, always UTC, so text order is time order
const TIMESTAMP_PRECISION: SecondsFormat = SecondsFormat::Micros;
const NANOS_PER_MICROSECOND: u32 = 1_000;

const LIFECYCLE_OPEN: &str = "open";
const LIFECYCLE_COMPLETED: &str = "completed";
const LIFECYCLE_TRASHED: &str = "trashed";
const CHANNEL_TUI: &str = "TUI";
const CHANNEL_DESKTOP: &str = "DESKTOP";
const CHANNEL_WEBHOOK: &str = "WEBHOOK";
const REMINDER_ACTIVE: &str = "active";
const REMINDER_PAUSED: &str = "paused";
const REMINDER_CANCELLED: &str = "cancelled";
const DELIVERY_PENDING: &str = "pending";
const DELIVERY_SENT: &str = "sent";
const DELIVERY_FAILED: &str = "failed";
const FREQUENCY_DAILY: &str = "DAILY";
const FREQUENCY_WEEKLY: &str = "WEEKLY";
const FREQUENCY_MONTHLY: &str = "MONTHLY";

/// One embedded schema migration.
///
/// `checksum` pins the exact SQL that was applied, and `tables` names what the migration
/// is responsible for creating, so a ledger recording a version whose tables are absent
/// is caught rather than skipped.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
    pub checksum: &'static str,
    pub tables: &'static [&'static str],
}

pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "remindr_authority",
    sql: AUTHORITY_MIGRATION,
    checksum: "d6ca601c121e83677e761e35a442d4d3df86438e0cb8cf0a5cd3df749f67fe22",
    tables: &[
        "projects",
        "tags",
        "todos",
        "todo_tags",
        "todo_parents",
        "todo_dependencies",
        "todo_recurrence",
        "todo_reminders",
        "todo_reminder_deliveries",
        "mg_remindr_authority_state",
    ],
}];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationState {
    pub version: i64,
    pub name: String,
    pub applied: bool,
}

/// One consistent view of all currently persisted authority state.
#[derive(Debug, Clone)]
pub struct AuthorityExport {
    pub projects: Vec<Project>,
    pub tags: Vec<Tag>,
    pub todos: Vec<Todo>,
    pub revision: u64,
}

/// Stable storage failures. Driver errors are intentionally redacted so paths and
/// driver text cannot enter display or debug output.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StorageError {
    #[error("mg-remindr database connection failed")]
    Connect,
    #[error("mg-remindr database operation failed during {operation}")]
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
    #[error("{field} must be exactly representable at microsecond precision")]
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

// Report a driver failure as the operation that asked for it, never as driver text
fn database(operation: &'static str) -> impl Fn(rusqlite::Error) -> StorageError {
    move |_| StorageError::Database { operation }
}

// A primary-key or UNIQUE clash means the caller's record is already here
fn is_unique_violation(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(failure, _) => matches!(
            failure.extended_code,
            rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY | rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
        ),
        _ => false,
    }
}

// ── The store ──

/// One SQLite file holding the whole authority.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// Open the store at one path and apply every migration it has not recorded.
    ///
    /// # Errors
    /// Returns an error when the file cannot be opened or its ledger disagrees with
    /// the embedded migrations.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let store = Self::attach(path)?;
        let mut connection = store.conn()?;
        apply_migrations(&mut connection)?;
        Ok(store)
    }

    /// The file this store lives in.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    // Open the file and settle its journal mode without applying anything
    fn attach(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|_| StorageError::Connect)?;
        }
        let store = Self { path };
        let connection = store.conn()?;
        // WAL lets a reader work while a writer holds the file; the mode is stored in the file
        let mode: String = connection
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(|_| StorageError::Connect)?;
        if !mode.eq_ignore_ascii_case(JOURNAL_MODE_WAL) {
            return Err(StorageError::Connect);
        }
        Ok(store)
    }

    // One connection per call, with the pragmas that are not stored in the file
    fn conn(&self) -> Result<Connection, StorageError> {
        let connection = Connection::open(&self.path).map_err(|_| StorageError::Connect)?;
        connection
            .busy_timeout(BUSY_TIMEOUT)
            .map_err(|_| StorageError::Connect)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|_| StorageError::Connect)?;
        Ok(connection)
    }
}

// ── Migrations ──

/// Read migration state without creating or changing the ledger.
///
/// # Errors
/// Returns an error when the file cannot be opened or its ledger disagrees with
/// the embedded migrations.
pub fn migration_status(path: &Path) -> Result<Vec<MigrationState>, StorageError> {
    const OPERATION: &str = "migration status";
    let store = Store::attach(path)?;
    let connection = store.conn()?;
    validate_migration_sources()?;
    let recorded = recorded_migrations(&connection, OPERATION)?;
    validate_recorded_migrations(&recorded)?;
    verify_recorded_tables(&connection, &recorded, OPERATION)?;
    verify_unledgered_tables(&connection, &recorded, OPERATION)?;
    Ok(MIGRATIONS
        .iter()
        .map(|migration| MigrationState {
            version: migration.version,
            name: migration.name.to_owned(),
            applied: recorded
                .iter()
                .any(|(version, ..)| *version == migration.version),
        })
        .collect())
}

/// Apply every pending embedded migration; rerunning is idempotent.
///
/// # Errors
/// Returns an error when the file cannot be opened, the ledger disagrees with the
/// embedded migrations, or a migration fails to apply.
pub fn migrate(path: &Path) -> Result<Vec<MigrationState>, StorageError> {
    let store = Store::attach(path)?;
    let mut connection = store.conn()?;
    apply_migrations(&mut connection)?;
    migration_status(path)
}

// Ledger discovery and every pending migration share one write transaction, so two
// first-time opens cannot both see an empty ledger and race to create the schema
fn apply_migrations(connection: &mut Connection) -> Result<(), StorageError> {
    const OPERATION: &str = "migration";
    validate_migration_sources()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database(OPERATION))?;
    transaction
        .execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {LEDGER} (\
             version INTEGER PRIMARY KEY, \
             name TEXT NOT NULL, \
             checksum TEXT NOT NULL, \
             applied_at TEXT NOT NULL);"
        ))
        .map_err(database(OPERATION))?;
    let recorded = recorded_migrations(&transaction, OPERATION)?;
    validate_recorded_migrations(&recorded)?;
    verify_recorded_tables(&transaction, &recorded, OPERATION)?;
    verify_unledgered_tables(&transaction, &recorded, OPERATION)?;
    for migration in MIGRATIONS {
        if recorded
            .iter()
            .any(|(version, ..)| *version == migration.version)
        {
            continue;
        }
        transaction
            .execute_batch(migration.sql)
            .map_err(database(OPERATION))?;
        transaction
            .execute(
                &format!(
                    "INSERT INTO {LEDGER} (version, name, checksum, applied_at) \
                     VALUES (?1, ?2, ?3, ?4)"
                ),
                params![
                    migration.version,
                    migration.name,
                    migration.checksum,
                    timestamp_text(Utc::now())
                ],
            )
            .map_err(database(OPERATION))?;
    }
    transaction.commit().map_err(database(OPERATION))?;
    Ok(())
}

// What the ledger says, in version order; an absent ledger has recorded nothing
fn recorded_migrations(
    connection: &Connection,
    operation: &'static str,
) -> Result<Vec<(i64, String, String)>, StorageError> {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![LEDGER],
            |row| row.get(0),
        )
        .map_err(database(operation))?;
    if !exists {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare(&format!(
            "SELECT version, name, checksum FROM {LEDGER} ORDER BY version"
        ))
        .map_err(database(operation))?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(database(operation))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(database(operation))
}

// Refuse a ledger that names a migration this build does not carry, renames one,
// records a different SQL body, or skips a version
fn validate_recorded_migrations(recorded: &[(i64, String, String)]) -> Result<(), StorageError> {
    for (version, name, checksum) in recorded {
        let Some(expected) = MIGRATIONS
            .iter()
            .find(|migration| migration.version == *version)
        else {
            return Err(StorageError::UnknownMigration {
                version: *version,
                name: name.clone(),
            });
        };
        if name != expected.name {
            return Err(StorageError::MigrationDrift {
                version: *version,
                expected: expected.name,
                actual: name.clone(),
            });
        }
        if checksum != expected.checksum {
            return Err(StorageError::MigrationChecksumDrift { version: *version });
        }
    }
    for (index, (applied_version, ..)) in recorded.iter().enumerate() {
        let expected = MIGRATIONS[index].version;
        if *applied_version != expected {
            return Err(StorageError::MigrationHistoryGap {
                missing_version: expected,
                applied_version: *applied_version,
            });
        }
    }
    Ok(())
}

// A recorded migration whose tables are gone is drift, not a store to keep writing to
fn verify_recorded_tables(
    connection: &Connection,
    recorded: &[(i64, String, String)],
    operation: &'static str,
) -> Result<(), StorageError> {
    for (version, ..) in recorded {
        let Some(migration) = MIGRATIONS
            .iter()
            .find(|migration| migration.version == *version)
        else {
            continue;
        };
        for table in migration.tables {
            if !table_exists(connection, table, operation)? {
                return Err(StorageError::MigrationSchemaDrift {
                    version: *version,
                    table,
                });
            }
        }
    }
    Ok(())
}

// A table this build owns that no ledger row explains was not put there by mg-remindr
fn verify_unledgered_tables(
    connection: &Connection,
    recorded: &[(i64, String, String)],
    operation: &'static str,
) -> Result<(), StorageError> {
    for migration in MIGRATIONS {
        if recorded
            .iter()
            .any(|(version, ..)| *version == migration.version)
        {
            continue;
        }
        for table in migration.tables {
            if table_exists(connection, table, operation)? {
                return Err(StorageError::UnledgeredMigrationTable { table });
            }
        }
    }
    Ok(())
}

fn table_exists(
    connection: &Connection,
    table: &str,
    operation: &'static str,
) -> Result<bool, StorageError> {
    connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![table],
            |row| row.get(0),
        )
        .map_err(database(operation))
}

// Refuse an embedded migration whose SQL no longer matches its recorded checksum.
// Without this the checksum is decoration: editing a migration and its checksum
// together would still let a rewritten migration reach a database.
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

// ── Projects ──

const PROJECT_COLUMNS: &str = "id, name, lifecycle, version, created_at, updated_at";

#[derive(Debug, Clone)]
pub struct ProjectRepository {
    store: Store,
}

impl ProjectRepository {
    #[must_use]
    pub const fn new(store: Store) -> Self {
        Self { store }
    }

    /// Insert a validated project without rewriting caller-owned identity or history.
    ///
    /// # Errors
    /// Returns an error when the project is invalid, already exists, or cannot be stored.
    pub fn create(&self, project: &Project) -> Result<(), StorageError> {
        const OPERATION: &str = "project create";
        revalidate(project)?;
        validate_timestamp_precision(project.created_at(), "created_at")?;
        validate_timestamp_precision(project.updated_at(), "updated_at")?;
        let version = database_version(project.version())?;
        let connection = self.store.conn()?;
        let result = connection.execute(
            &format!("INSERT INTO projects ({PROJECT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"),
            params![
                id_text(project.id().as_uuid()),
                project.name(),
                lifecycle_text(project.lifecycle()),
                version,
                timestamp_text(project.created_at()),
                timestamp_text(project.updated_at()),
            ],
        );
        match result {
            Ok(1) => Ok(()),
            Ok(_) => Err(StorageError::Database {
                operation: OPERATION,
            }),
            Err(error) if is_unique_violation(&error) => Err(StorageError::ProjectAlreadyExists {
                project_id: project.id(),
            }),
            Err(_) => Err(StorageError::Database {
                operation: OPERATION,
            }),
        }
    }

    /// Find one project by identity.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or holds invalid project data.
    pub fn find(&self, project_id: ProjectId) -> Result<Option<Project>, StorageError> {
        const OPERATION: &str = "project find";
        let connection = self.store.conn()?;
        connection
            .query_row(
                &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?1"),
                params![id_text(project_id.as_uuid())],
                |row| Ok(project_from_row(row)),
            )
            .optional()
            .map_err(database(OPERATION))?
            .transpose()
    }

    /// List every project in deterministic ID order without changing authority.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or holds invalid project data.
    pub fn list(&self) -> Result<Vec<Project>, StorageError> {
        const OPERATION: &str = "project list";
        let connection = self.store.conn()?;
        read_projects(&connection, OPERATION)
    }

    /// Replace one project under an immediate transaction and the caller's optimistic version.
    ///
    /// # Errors
    /// Returns an error when the replacement is invalid, the project is missing, or the
    /// stored version is not the expected one.
    pub fn replace(&self, expected: Version, replacement: &Project) -> Result<(), StorageError> {
        const OPERATION: &str = "project replace";
        revalidate(replacement)?;
        let mut connection = self.store.conn()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database(OPERATION))?;
        let current = transaction
            .query_row(
                &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?1"),
                params![id_text(replacement.id().as_uuid())],
                |row| Ok(project_from_row(row)),
            )
            .optional()
            .map_err(database(OPERATION))?
            .ok_or(StorageError::ProjectNotFound {
                project_id: replacement.id(),
            })??;
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
        let changed = transaction
            .execute(
                "UPDATE projects SET name = ?1, lifecycle = ?2, version = ?3, updated_at = ?4 \
                 WHERE id = ?5 AND version = ?6",
                params![
                    replacement.name(),
                    lifecycle_text(replacement.lifecycle()),
                    database_version(replacement.version())?,
                    timestamp_text(replacement.updated_at()),
                    id_text(replacement.id().as_uuid()),
                    database_version(expected)?,
                ],
            )
            .map_err(database(OPERATION))?;
        if changed != 1 {
            return Err(StorageError::Database {
                operation: OPERATION,
            });
        }
        transaction.commit().map_err(database(OPERATION))
    }
}

fn read_projects(
    connection: &Connection,
    operation: &'static str,
) -> Result<Vec<Project>, StorageError> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT {PROJECT_COLUMNS} FROM projects ORDER BY id"
        ))
        .map_err(database(operation))?;
    let mut rows = statement.query([]).map_err(database(operation))?;
    let mut projects = Vec::new();
    while let Some(row) = rows.next().map_err(database(operation))? {
        projects.push(project_from_row(row)?);
    }
    Ok(projects)
}

// ── Tags ──

const TAG_COLUMNS: &str = "id, name, version, created_at, updated_at";

#[derive(Debug, Clone)]
pub struct TagRepository {
    store: Store,
}

impl TagRepository {
    #[must_use]
    pub const fn new(store: Store) -> Self {
        Self { store }
    }

    /// Insert a validated tag without rewriting caller-owned identity or history.
    ///
    /// # Errors
    /// Returns an error when the tag is invalid, already exists, or cannot be stored.
    pub fn create(&self, tag: &Tag) -> Result<(), StorageError> {
        const OPERATION: &str = "tag create";
        revalidate_tag(tag)?;
        validate_timestamp_precision(tag.created_at(), "created_at")?;
        validate_timestamp_precision(tag.updated_at(), "updated_at")?;
        let version = tag_creation_database_version(tag.version())?;
        let connection = self.store.conn()?;
        let result = connection.execute(
            &format!("INSERT INTO tags ({TAG_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5)"),
            params![
                id_text(tag.id().as_uuid()),
                tag.name(),
                version,
                timestamp_text(tag.created_at()),
                timestamp_text(tag.updated_at()),
            ],
        );
        match result {
            Ok(1) => Ok(()),
            Ok(_) => Err(StorageError::Database {
                operation: OPERATION,
            }),
            Err(error) if is_unique_violation(&error) => {
                Err(StorageError::TagAlreadyExists { tag_id: tag.id() })
            }
            Err(_) => Err(StorageError::Database {
                operation: OPERATION,
            }),
        }
    }

    /// Find one tag by identity.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or holds invalid tag data.
    pub fn find(&self, tag_id: TagId) -> Result<Option<Tag>, StorageError> {
        const OPERATION: &str = "tag find";
        let connection = self.store.conn()?;
        connection
            .query_row(
                &format!("SELECT {TAG_COLUMNS} FROM tags WHERE id = ?1"),
                params![id_text(tag_id.as_uuid())],
                |row| Ok(tag_from_row(row)),
            )
            .optional()
            .map_err(database(OPERATION))?
            .transpose()
    }

    /// List every tag in deterministic ID order without changing authority.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or holds invalid tag data.
    pub fn list(&self) -> Result<Vec<Tag>, StorageError> {
        const OPERATION: &str = "tag list";
        let connection = self.store.conn()?;
        read_tags(&connection, OPERATION)
    }

    /// Replace one tag under an immediate transaction and the caller's optimistic version.
    ///
    /// # Errors
    /// Returns an error when the replacement is invalid, the tag is missing, or the
    /// stored version is not the expected one.
    pub fn replace(&self, expected: Version, replacement: &Tag) -> Result<(), StorageError> {
        const OPERATION: &str = "tag replace";
        revalidate_tag(replacement)?;
        let mut connection = self.store.conn()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database(OPERATION))?;
        let current = transaction
            .query_row(
                &format!("SELECT {TAG_COLUMNS} FROM tags WHERE id = ?1"),
                params![id_text(replacement.id().as_uuid())],
                |row| Ok(tag_from_row(row)),
            )
            .optional()
            .map_err(database(OPERATION))?
            .ok_or(StorageError::TagNotFound {
                tag_id: replacement.id(),
            })??;
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
                "UPDATE tags SET name = ?1, version = ?2, updated_at = ?3 \
                 WHERE id = ?4 AND version = ?5",
                params![
                    replacement.name(),
                    tag_database_version(replacement.version())?,
                    timestamp_text(replacement.updated_at()),
                    id_text(replacement.id().as_uuid()),
                    tag_database_version(expected)?,
                ],
            )
            .map_err(database(OPERATION))?;
        if changed != 1 {
            return Err(StorageError::Database {
                operation: OPERATION,
            });
        }
        transaction.commit().map_err(database(OPERATION))
    }
}

fn read_tags(connection: &Connection, operation: &'static str) -> Result<Vec<Tag>, StorageError> {
    let mut statement = connection
        .prepare(&format!("SELECT {TAG_COLUMNS} FROM tags ORDER BY id"))
        .map_err(database(operation))?;
    let mut rows = statement.query([]).map_err(database(operation))?;
    let mut tags = Vec::new();
    while let Some(row) = rows.next().map_err(database(operation))? {
        tags.push(tag_from_row(row)?);
    }
    Ok(tags)
}

// ── Todos ──

const TODO_COLUMNS: &str = "id, title, project_id, lifecycle, version, created_at, updated_at, \
     completed_at, trashed_at, due_date, due_at, due_timezone";

/// Authority for core todo state. Relationship, recurrence, and reminder fields are
/// rejected on replacement until those contracts join the todo aggregate.
#[derive(Debug, Clone)]
pub struct TodoRepository {
    store: Store,
}

impl TodoRepository {
    #[must_use]
    pub const fn new(store: Store) -> Self {
        Self { store }
    }

    /// Insert a validated todo, its relationships, and its due value together.
    ///
    /// # Errors
    /// Returns an error when the todo is invalid, already exists, names a missing
    /// project or relationship, or cannot be stored.
    pub fn create(&self, todo: &Todo) -> Result<(), StorageError> {
        const OPERATION: &str = "todo create";
        revalidate_todo(todo)?;
        validate_timestamp_precision(todo.created_at(), "created_at")?;
        validate_timestamp_precision(todo.updated_at(), "updated_at")?;
        let version = todo_creation_database_version(todo.version())?;
        let mut connection = self.store.conn()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database(OPERATION))?;
        validate_todo_project(&transaction, todo.project_id())?;
        validate_todo_relationships(&transaction, todo)?;
        let due = DueColumns::from(todo.due());
        let result = transaction.execute(
            &format!(
                "INSERT INTO todos ({TODO_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
            ),
            params![
                id_text(todo.id().as_uuid()),
                todo.title(),
                todo.project_id().map(|id| id_text(id.as_uuid())),
                lifecycle_text(todo.lifecycle()),
                version,
                timestamp_text(todo.created_at()),
                timestamp_text(todo.updated_at()),
                todo.completed_at().map(timestamp_text),
                todo.trashed_at().map(timestamp_text),
                due.date,
                due.at,
                due.timezone,
            ],
        );
        match result {
            Ok(1) => {
                persist_todo_relationships(&transaction, todo)?;
                transaction.commit().map_err(database(OPERATION))
            }
            Ok(_) => Err(StorageError::Database {
                operation: OPERATION,
            }),
            Err(error) if is_unique_violation(&error) => {
                Err(StorageError::TodoAlreadyExists { todo_id: todo.id() })
            }
            Err(_) => Err(StorageError::Database {
                operation: OPERATION,
            }),
        }
    }

    /// Find one todo, with the relationships stored alongside it.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or holds invalid todo data.
    pub fn find(&self, todo_id: TodoId) -> Result<Option<Todo>, StorageError> {
        const OPERATION: &str = "todo find";
        let connection = self.store.conn()?;
        let todo = connection
            .query_row(
                &format!("SELECT {TODO_COLUMNS} FROM todos WHERE id = ?1"),
                params![id_text(todo_id.as_uuid())],
                |row| Ok(todo_from_row(row)),
            )
            .optional()
            .map_err(database(OPERATION))?
            .transpose()?;
        match todo {
            Some(todo) => load_todo_relationships(&connection, todo).map(Some),
            None => Ok(None),
        }
    }

    /// List core todos in deterministic ID order.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or holds invalid todo data.
    pub fn list(&self) -> Result<Vec<Todo>, StorageError> {
        const OPERATION: &str = "todo list";
        let connection = self.store.conn()?;
        read_todos(&connection, OPERATION)
    }

    /// Replace one todo under an immediate transaction and the caller's optimistic version.
    ///
    /// # Errors
    /// Returns an error when the replacement is invalid, the todo is missing, or the
    /// stored version is not the expected one.
    pub fn replace(&self, expected: Version, replacement: &Todo) -> Result<(), StorageError> {
        const OPERATION: &str = "todo replace";
        revalidate_todo(replacement)?;
        validate_todo_foundation(replacement)
            .map_err(|reason| StorageError::InvalidTodoReplacement { reason })?;
        let mut connection = self.store.conn()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database(OPERATION))?;
        let stored = transaction
            .query_row(
                &format!("SELECT {TODO_COLUMNS} FROM todos WHERE id = ?1"),
                params![id_text(replacement.id().as_uuid())],
                |row| Ok(todo_from_row(row)),
            )
            .optional()
            .map_err(database(OPERATION))?
            .ok_or(StorageError::TodoNotFound {
                todo_id: replacement.id(),
            })??;
        let current = load_todo_relationships(&transaction, stored)?;
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
        validate_todo_project(&transaction, replacement.project_id())?;
        validate_todo_relationships(&transaction, replacement)?;
        let due = DueColumns::from(replacement.due());
        let changed = transaction
            .execute(
                "UPDATE todos SET title = ?1, project_id = ?2, lifecycle = ?3, version = ?4, \
                 updated_at = ?5, completed_at = ?6, trashed_at = ?7, due_date = ?8, \
                 due_at = ?9, due_timezone = ?10 WHERE id = ?11 AND version = ?12",
                params![
                    replacement.title(),
                    replacement.project_id().map(|id| id_text(id.as_uuid())),
                    lifecycle_text(replacement.lifecycle()),
                    todo_database_version(replacement.version())?,
                    timestamp_text(replacement.updated_at()),
                    replacement.completed_at().map(timestamp_text),
                    replacement.trashed_at().map(timestamp_text),
                    due.date,
                    due.at,
                    due.timezone,
                    id_text(replacement.id().as_uuid()),
                    todo_database_version(expected)?,
                ],
            )
            .map_err(database(OPERATION))?;
        if changed != 1 {
            return Err(StorageError::Database {
                operation: OPERATION,
            });
        }
        persist_todo_relationships(&transaction, replacement)?;
        transaction.commit().map_err(database(OPERATION))
    }
}

fn read_todos(connection: &Connection, operation: &'static str) -> Result<Vec<Todo>, StorageError> {
    let mut statement = connection
        .prepare(&format!("SELECT {TODO_COLUMNS} FROM todos ORDER BY id"))
        .map_err(database(operation))?;
    let mut rows = statement.query([]).map_err(database(operation))?;
    let mut stored = Vec::new();
    while let Some(row) = rows.next().map_err(database(operation))? {
        stored.push(todo_from_row(row)?);
    }
    stored
        .into_iter()
        .map(|todo| load_todo_relationships(connection, todo))
        .collect()
}

// ── Todo relationships ──

// Every named parent, dependency, and tag must exist, and no edge may close a cycle
fn validate_todo_relationships(connection: &Connection, todo: &Todo) -> Result<(), StorageError> {
    const PARENT_OPERATION: &str = "todo parent validation";
    const DEPENDENCY_OPERATION: &str = "todo dependency validation";
    const TAG_OPERATION: &str = "todo tag validation";
    if let Some(parent) = todo.parent_id() {
        if parent == todo.id() {
            return Err(StorageError::TodoRelationshipConflict {
                reason: "todo cannot be its own parent",
            });
        }
        if !todo_exists(connection, parent, PARENT_OPERATION)? {
            return Err(StorageError::TodoRelationshipConflict {
                reason: "parent todo was not found",
            });
        }
        let cycle: bool = connection
            .query_row(
                "WITH RECURSIVE ancestors(id) AS (\
                 SELECT ?1 UNION SELECT p.parent_id FROM todo_parents p \
                 JOIN ancestors a ON p.child_id = a.id) \
                 SELECT EXISTS (SELECT 1 FROM ancestors WHERE id = ?2)",
                params![id_text(parent.as_uuid()), id_text(todo.id().as_uuid())],
                |row| row.get(0),
            )
            .map_err(database(PARENT_OPERATION))?;
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
        if !todo_exists(connection, *dependency, DEPENDENCY_OPERATION)? {
            return Err(StorageError::TodoRelationshipConflict {
                reason: "dependency todo was not found",
            });
        }
        let cycle: bool = connection
            .query_row(
                "WITH RECURSIVE reach(id) AS (\
                 SELECT ?1 UNION SELECT d.prerequisite_id FROM todo_dependencies d \
                 JOIN reach r ON d.dependent_id = r.id) \
                 SELECT EXISTS (SELECT 1 FROM reach WHERE id = ?2)",
                params![id_text(dependency.as_uuid()), id_text(todo.id().as_uuid())],
                |row| row.get(0),
            )
            .map_err(database(DEPENDENCY_OPERATION))?;
        if cycle {
            return Err(StorageError::TodoRelationshipConflict {
                reason: "dependency relationship would create a cycle",
            });
        }
    }
    for tag in todo.tag_ids() {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM tags WHERE id = ?1)",
                params![id_text(tag.as_uuid())],
                |row| row.get(0),
            )
            .map_err(database(TAG_OPERATION))?;
        if !exists {
            return Err(StorageError::TodoRelationshipConflict {
                reason: "tag was not found",
            });
        }
    }
    Ok(())
}

fn todo_exists(
    connection: &Connection,
    todo_id: TodoId,
    operation: &'static str,
) -> Result<bool, StorageError> {
    connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM todos WHERE id = ?1)",
            params![id_text(todo_id.as_uuid())],
            |row| row.get(0),
        )
        .map_err(database(operation))
}

// The stored edges are whatever the caller's todo says they are, replaced wholesale
fn persist_todo_relationships(connection: &Connection, todo: &Todo) -> Result<(), StorageError> {
    const OPERATION: &str = "todo relationship write";
    let id = id_text(todo.id().as_uuid());
    for statement in [
        "DELETE FROM todo_parents WHERE child_id = ?1",
        "DELETE FROM todo_dependencies WHERE dependent_id = ?1",
        "DELETE FROM todo_tags WHERE todo_id = ?1",
    ] {
        connection
            .execute(statement, params![id])
            .map_err(database(OPERATION))?;
    }
    if let Some(parent) = todo.parent_id() {
        connection
            .execute(
                "INSERT INTO todo_parents (child_id, parent_id) VALUES (?1, ?2)",
                params![id, id_text(parent.as_uuid())],
            )
            .map_err(database(OPERATION))?;
    }
    for dependency in todo.dependency_ids() {
        connection
            .execute(
                "INSERT INTO todo_dependencies (dependent_id, prerequisite_id) VALUES (?1, ?2)",
                params![id, id_text(dependency.as_uuid())],
            )
            .map_err(database(OPERATION))?;
    }
    for tag in todo.tag_ids() {
        connection
            .execute(
                "INSERT INTO todo_tags (todo_id, tag_id) VALUES (?1, ?2)",
                params![id, id_text(tag.as_uuid())],
            )
            .map_err(database(OPERATION))?;
    }
    Ok(())
}

// A todo read back carries the edges the store holds, not the ones the row alone knows
fn load_todo_relationships(connection: &Connection, todo: Todo) -> Result<Todo, StorageError> {
    const OPERATION: &str = "todo relationship read";
    let id = id_text(todo.id().as_uuid());
    let parent = connection
        .query_row(
            "SELECT parent_id FROM todo_parents WHERE child_id = ?1",
            params![id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(database(OPERATION))?
        .map(|value| parse_id(&value).map(TodoId::from_uuid))
        .transpose()?;
    let dependencies = read_ids(
        connection,
        "SELECT prerequisite_id FROM todo_dependencies WHERE dependent_id = ?1 \
         ORDER BY prerequisite_id",
        &id,
        OPERATION,
    )?
    .into_iter()
    .map(TodoId::from_uuid)
    .collect();
    let tags = read_ids(
        connection,
        "SELECT tag_id FROM todo_tags WHERE todo_id = ?1 ORDER BY tag_id",
        &id,
        OPERATION,
    )?
    .into_iter()
    .map(TagId::from_uuid)
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
        todo.completed_at(),
        todo.trashed_at(),
        todo.due().cloned(),
    )
    .map_err(StorageError::Domain)
}

fn read_ids(
    connection: &Connection,
    query: &str,
    id: &str,
    operation: &'static str,
) -> Result<Vec<Uuid>, StorageError> {
    let mut statement = connection.prepare(query).map_err(database(operation))?;
    let rows = statement
        .query_map(params![id], |row| row.get::<_, String>(0))
        .map_err(database(operation))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(database(operation))?
        .iter()
        .map(|value| parse_id(value))
        .collect()
}

fn validate_todo_project(
    connection: &Connection,
    project_id: Option<ProjectId>,
) -> Result<(), StorageError> {
    const OPERATION: &str = "todo project validation";
    let Some(project_id) = project_id else {
        return Ok(());
    };
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM projects WHERE id = ?1)",
            params![id_text(project_id.as_uuid())],
            |row| row.get(0),
        )
        .map_err(database(OPERATION))?;
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

// ── Reminders, deliveries, recurrence ──

/// Persist a validated reminder.
///
/// # Errors
/// Returns an error when the reminder is invalid or cannot be stored.
pub fn create_reminder(store: &Store, reminder: &Reminder) -> Result<(), StorageError> {
    const OPERATION: &str = "reminder create";
    reminder.validate()?;
    validate_timestamp_precision(reminder.remind_at, "remind_at")?;
    validate_timestamp_precision(reminder.created_at, "created_at")?;
    validate_timestamp_precision(reminder.updated_at, "updated_at")?;
    let version = i64::try_from(reminder.version).map_err(|_| StorageError::Database {
        operation: OPERATION,
    })?;
    let connection = store.conn()?;
    let changed = connection
        .execute(
            "INSERT INTO todo_reminders \
             (id, todo_id, remind_at, channel, lifecycle, version, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id_text(reminder.id.as_uuid()),
                id_text(reminder.todo_id.as_uuid()),
                timestamp_text(reminder.remind_at),
                channel_text(reminder.channel),
                reminder_lifecycle_text(reminder.lifecycle),
                version,
                timestamp_text(reminder.created_at),
                timestamp_text(reminder.updated_at),
            ],
        )
        .map_err(database(OPERATION))?;
    if changed != 1 {
        return Err(StorageError::Database {
            operation: OPERATION,
        });
    }
    Ok(())
}

/// Persist a pending, sent, or failed delivery record.
///
/// # Errors
/// Returns an error when the record is invalid or cannot be stored.
pub fn create_delivery_record(store: &Store, record: &DeliveryRecord) -> Result<(), StorageError> {
    const OPERATION: &str = "delivery create";
    record.validate()?;
    validate_timestamp_precision(record.created_at, "created_at")?;
    if let Some(attempted_at) = record.attempted_at {
        validate_timestamp_precision(attempted_at, "attempted_at")?;
    }
    let connection = store.conn()?;
    let changed = connection
        .execute(
            "INSERT INTO todo_reminder_deliveries \
             (id, reminder_id, idempotency_key, status, attempted_at, provider_reference, \
             failure_code, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id_text(record.id.as_uuid()),
                id_text(record.reminder_id.as_uuid()),
                record.idempotency_key,
                delivery_status_text(record.status),
                record.attempted_at.map(timestamp_text),
                record.provider_reference,
                record.failure_code,
                timestamp_text(record.created_at),
            ],
        )
        .map_err(database(OPERATION))?;
    if changed != 1 {
        return Err(StorageError::Database {
            operation: OPERATION,
        });
    }
    Ok(())
}

/// Replace the bounded recurrence rule for one authoritative todo.
///
/// # Errors
/// Returns an error when the rule is invalid, the todo is missing, or the write fails.
pub fn set_todo_recurrence(
    store: &Store,
    todo_id: TodoId,
    start: NaiveDate,
    rule: &Rule,
) -> Result<(), StorageError> {
    const OPERATION: &str = "recurrence set";
    rule.validate(start)?;
    let mut connection = store.conn()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database(OPERATION))?;
    if !todo_exists(&transaction, todo_id, OPERATION)? {
        return Err(StorageError::InvalidStoredTodoData);
    }
    transaction
        .execute(
            "INSERT INTO todo_recurrence \
             (todo_id, start_date, frequency, interval, occurrence_count, until_date) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT (todo_id) DO UPDATE SET start_date = excluded.start_date, \
             frequency = excluded.frequency, interval = excluded.interval, \
             occurrence_count = excluded.occurrence_count, until_date = excluded.until_date",
            params![
                id_text(todo_id.as_uuid()),
                date_text(start),
                frequency_text(rule.frequency),
                i64::from(rule.interval),
                rule.count.map(i64::from),
                rule.until.map(date_text),
            ],
        )
        .map_err(database(OPERATION))?;
    transaction.commit().map_err(database(OPERATION))
}

/// Read the authoritative recurrence rule for one todo.
///
/// # Errors
/// Returns an error when the store cannot be read or holds an invalid rule.
pub fn find_todo_recurrence(
    store: &Store,
    todo_id: TodoId,
) -> Result<Option<(NaiveDate, Rule)>, StorageError> {
    const OPERATION: &str = "recurrence find";
    let connection = store.conn()?;
    let row = connection
        .query_row(
            "SELECT start_date, frequency, interval, occurrence_count, until_date \
             FROM todo_recurrence WHERE todo_id = ?1",
            params![id_text(todo_id.as_uuid())],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(database(OPERATION))?;
    let Some((start, frequency, interval, count, until)) = row else {
        return Ok(None);
    };
    let start = parse_date(&start)?;
    let frequency = match frequency.as_str() {
        FREQUENCY_DAILY => Frequency::Daily,
        FREQUENCY_WEEKLY => Frequency::Weekly,
        FREQUENCY_MONTHLY => Frequency::Monthly,
        _ => return Err(StorageError::InvalidStoredTodoData),
    };
    let interval = u32::try_from(interval).map_err(|_| StorageError::InvalidStoredTodoData)?;
    let count = count
        .map(u32::try_from)
        .transpose()
        .map_err(|_| StorageError::InvalidStoredTodoData)?;
    let until = until.as_deref().map(parse_date).transpose()?;
    let rule = Rule::new(frequency, interval, count, until, start)?;
    Ok(Some((start, rule)))
}

// ── Export ──

/// Read the complete currently representable authority in one transaction.
///
/// Relationship, recurrence, and reminder rows are not fabricated here; each must be
/// added to the export contract with its own migration and tests.
///
/// # Errors
/// Returns an error when the store cannot be read or holds invalid data.
pub fn export_authority(store: &Store) -> Result<AuthorityExport, StorageError> {
    const OPERATION: &str = "interop export";
    let mut connection = store.conn()?;
    // One transaction, so the revision and the rows it counts are the same instant
    let transaction = connection.transaction().map_err(database(OPERATION))?;
    let projects = read_projects(&transaction, OPERATION)?;
    let tags = read_tags(&transaction, OPERATION)?;
    let todos = read_todos(&transaction, OPERATION)?;
    let revision: i64 = transaction
        .query_row(
            "SELECT revision FROM mg_remindr_authority_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(database(OPERATION))?
        .ok_or(StorageError::Database {
            operation: OPERATION,
        })?;
    let revision = u64::try_from(revision).map_err(|_| StorageError::Database {
        operation: OPERATION,
    })?;
    transaction.commit().map_err(database(OPERATION))?;
    Ok(AuthorityExport {
        projects,
        tags,
        todos,
        revision,
    })
}

/// Raise the authority checkpoint to at least `revision`, leaving it alone otherwise.
///
/// Used when rows arrive from somewhere the triggers never saw, so a consumer that
/// remembers the last revision it read is never handed a lower one.
///
/// # Errors
/// Returns an error when the checkpoint cannot be written.
pub fn raise_authority_revision(store: &Store, revision: u64) -> Result<u64, StorageError> {
    const OPERATION: &str = "authority revision";
    let wanted = i64::try_from(revision).map_err(|_| StorageError::Database {
        operation: OPERATION,
    })?;
    let mut connection = store.conn()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database(OPERATION))?;
    transaction
        .execute(
            "UPDATE mg_remindr_authority_state SET revision = ?1, changed_at = ?2 \
             WHERE singleton = 1 AND revision < ?1",
            params![wanted, timestamp_text(Utc::now())],
        )
        .map_err(database(OPERATION))?;
    let current: i64 = transaction
        .query_row(
            "SELECT revision FROM mg_remindr_authority_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(database(OPERATION))?;
    transaction.commit().map_err(database(OPERATION))?;
    u64::try_from(current).map_err(|_| StorageError::Database {
        operation: OPERATION,
    })
}

// ── Rows in, rows out ──

/// The three nullable columns one due value occupies.
struct DueColumns {
    date: Option<String>,
    at: Option<String>,
    timezone: Option<String>,
}

impl From<Option<&TodoDue>> for DueColumns {
    fn from(due: Option<&TodoDue>) -> Self {
        match due {
            None => Self {
                date: None,
                at: None,
                timezone: None,
            },
            Some(TodoDue::Date { date, timezone }) => Self {
                date: Some(date_text(*date)),
                at: None,
                timezone: Some(timezone.clone()),
            },
            Some(TodoDue::Timed { at, timezone }) => Self {
                date: None,
                at: Some(timestamp_text(at.with_timezone(&Utc))),
                timezone: Some(timezone.clone()),
            },
        }
    }
}

// The stored instant is reprojected into its own zone, so the exported offset is the zone's
fn due_from_row(row: &Row<'_>) -> Result<Option<TodoDue>, StorageError> {
    let date = column_text(row, 9)?;
    let at = column_text(row, 10)?;
    let timezone = column_text(row, 11)?;
    match (date, at, timezone) {
        (None, None, None) => Ok(None),
        (Some(date), None, Some(timezone)) => TodoDue::date(parse_date(&date)?, timezone)
            .map(Some)
            .map_err(|_| StorageError::InvalidStoredTodoData),
        (None, Some(at), Some(timezone)) => {
            let zone = timezone
                .parse::<Tz>()
                .map_err(|_| StorageError::InvalidStoredTodoData)?;
            let zoned = parse_timestamp(&at)?.with_timezone(&zone);
            TodoDue::timed(zoned.fixed_offset(), timezone)
                .map(Some)
                .map_err(|_| StorageError::InvalidStoredTodoData)
        }
        _ => Err(StorageError::InvalidStoredTodoData),
    }
}

fn project_from_row(row: &Row<'_>) -> Result<Project, StorageError> {
    let lifecycle = parse_lifecycle(&required_text(row, 2, StorageError::InvalidStoredData)?)
        .ok_or(StorageError::InvalidStoredData)?;
    Project::new(
        ProjectId::from_uuid(parse_id(&required_text(
            row,
            0,
            StorageError::InvalidStoredData,
        )?)?),
        required_text(row, 1, StorageError::InvalidStoredData)?,
        lifecycle,
        parse_version(row, 3, StorageError::InvalidStoredData)?,
        parse_timestamp(&required_text(row, 4, StorageError::InvalidStoredData)?)?,
        parse_timestamp(&required_text(row, 5, StorageError::InvalidStoredData)?)?,
    )
    .map_err(|_| StorageError::InvalidStoredData)
}

fn tag_from_row(row: &Row<'_>) -> Result<Tag, StorageError> {
    Tag::new(
        TagId::from_uuid(parse_id(&required_text(
            row,
            0,
            StorageError::InvalidStoredTagData,
        )?)?),
        required_text(row, 1, StorageError::InvalidStoredTagData)?,
        parse_version(row, 2, StorageError::InvalidStoredTagData)?,
        parse_timestamp(&required_text(row, 3, StorageError::InvalidStoredTagData)?)?,
        parse_timestamp(&required_text(row, 4, StorageError::InvalidStoredTagData)?)?,
    )
    .map_err(|_| StorageError::InvalidStoredTagData)
}

fn todo_from_row(row: &Row<'_>) -> Result<Todo, StorageError> {
    let lifecycle = parse_lifecycle(&required_text(row, 3, StorageError::InvalidStoredTodoData)?)
        .ok_or(StorageError::InvalidStoredTodoData)?;
    let project_id = column_text(row, 2)?
        .map(|value| parse_id(&value).map(ProjectId::from_uuid))
        .transpose()?;
    Todo::new(
        TodoId::from_uuid(parse_id(&required_text(
            row,
            0,
            StorageError::InvalidStoredTodoData,
        )?)?),
        required_text(row, 1, StorageError::InvalidStoredTodoData)?,
        project_id,
        None,
        vec![],
        vec![],
        lifecycle,
        parse_version(row, 4, StorageError::InvalidStoredTodoData)?,
        parse_timestamp(&required_text(row, 5, StorageError::InvalidStoredTodoData)?)?,
        parse_timestamp(&required_text(row, 6, StorageError::InvalidStoredTodoData)?)?,
        column_text(row, 7)?
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
        column_text(row, 8)?
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
        due_from_row(row)?,
    )
    .map_err(|_| StorageError::InvalidStoredTodoData)
}

fn column_text(row: &Row<'_>, index: usize) -> Result<Option<String>, StorageError> {
    row.get::<_, Option<String>>(index)
        .map_err(|_| StorageError::InvalidStoredData)
}

fn required_text(
    row: &Row<'_>,
    index: usize,
    invalid: StorageError,
) -> Result<String, StorageError> {
    row.get::<_, String>(index).map_err(|_| invalid)
}

fn parse_version(
    row: &Row<'_>,
    index: usize,
    invalid: StorageError,
) -> Result<Version, StorageError> {
    let raw = row.get::<_, i64>(index).map_err(|_| invalid.clone())?;
    u64::try_from(raw)
        .ok()
        .and_then(|value| Version::try_from_value(value).ok())
        .ok_or(invalid)
}

fn parse_lifecycle(value: &str) -> Option<Lifecycle> {
    match value {
        LIFECYCLE_OPEN => Some(Lifecycle::Open),
        LIFECYCLE_COMPLETED => Some(Lifecycle::Completed),
        LIFECYCLE_TRASHED => Some(Lifecycle::Trashed),
        _ => None,
    }
}

const fn lifecycle_text(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Open => LIFECYCLE_OPEN,
        Lifecycle::Completed => LIFECYCLE_COMPLETED,
        Lifecycle::Trashed => LIFECYCLE_TRASHED,
    }
}

const fn channel_text(channel: Channel) -> &'static str {
    match channel {
        Channel::Tui => CHANNEL_TUI,
        Channel::Desktop => CHANNEL_DESKTOP,
        Channel::Webhook => CHANNEL_WEBHOOK,
    }
}

const fn reminder_lifecycle_text(lifecycle: ReminderLifecycle) -> &'static str {
    match lifecycle {
        ReminderLifecycle::Active => REMINDER_ACTIVE,
        ReminderLifecycle::Paused => REMINDER_PAUSED,
        ReminderLifecycle::Cancelled => REMINDER_CANCELLED,
    }
}

const fn delivery_status_text(status: DeliveryStatus) -> &'static str {
    match status {
        DeliveryStatus::Pending => DELIVERY_PENDING,
        DeliveryStatus::Sent => DELIVERY_SENT,
        DeliveryStatus::Failed => DELIVERY_FAILED,
    }
}

const fn frequency_text(frequency: Frequency) -> &'static str {
    match frequency {
        Frequency::Daily => FREQUENCY_DAILY,
        Frequency::Weekly => FREQUENCY_WEEKLY,
        Frequency::Monthly => FREQUENCY_MONTHLY,
    }
}

// ── Stored shapes ──

/// One identifier, written the one way the schema reads it.
#[must_use]
pub fn id_text(value: Uuid) -> String {
    value.hyphenated().to_string()
}

fn parse_id(value: &str) -> Result<Uuid, StorageError> {
    Uuid::parse_str(value).map_err(|_| StorageError::InvalidStoredData)
}

/// One instant, fixed width and always UTC, so text order is time order.
#[must_use]
pub fn timestamp_text(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(TIMESTAMP_PRECISION, true)
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| StorageError::InvalidStoredData)
}

/// One civil date, written the way a person writes one.
#[must_use]
pub fn date_text(value: NaiveDate) -> String {
    value.format(DATE_FORMAT).to_string()
}

fn parse_date(value: &str) -> Result<NaiveDate, StorageError> {
    NaiveDate::parse_from_str(value, DATE_FORMAT).map_err(|_| StorageError::InvalidStoredData)
}

// A stored instant is microseconds wide; anything finer would be silently truncated
fn validate_timestamp_precision(
    timestamp: DateTime<Utc>,
    field: &'static str,
) -> Result<(), StorageError> {
    if timestamp.timestamp_subsec_nanos() % NANOS_PER_MICROSECOND == 0 {
        Ok(())
    } else {
        Err(StorageError::InvalidTimestampPrecision { field })
    }
}

// ── Revalidation and version bounds ──

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
        todo.completed_at(),
        todo.trashed_at(),
        todo.due().cloned(),
    )?;
    Ok(())
}

fn database_version(version: Version) -> Result<i64, StorageError> {
    i64::try_from(version.value()).map_err(|_| StorageError::InvalidReplacement {
        reason: "version exceeds the stored integer range",
    })
}

fn tag_creation_database_version(version: Version) -> Result<i64, StorageError> {
    i64::try_from(version.value()).map_err(|_| StorageError::InvalidTagCreation {
        reason: "version exceeds the stored integer range",
    })
}

fn tag_database_version(version: Version) -> Result<i64, StorageError> {
    i64::try_from(version.value()).map_err(|_| StorageError::InvalidTagReplacement {
        reason: "version exceeds the stored integer range",
    })
}

fn todo_creation_database_version(version: Version) -> Result<i64, StorageError> {
    i64::try_from(version.value()).map_err(|_| StorageError::InvalidTodoCreation {
        reason: "version exceeds the stored integer range",
    })
}

fn todo_database_version(version: Version) -> Result<i64, StorageError> {
    i64::try_from(version.value()).map_err(|_| StorageError::InvalidTodoReplacement {
        reason: "version exceeds the stored integer range",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};
    use tempfile::TempDir;

    // A store of its own, in a directory that goes away with the test
    fn scratch() -> (TempDir, Store) {
        let directory = tempfile::tempdir().expect("create a scratch directory");
        let store = Store::open(directory.path().join("remindr.sqlite")).expect("open the store");
        (directory, store)
    }

    fn project(lifecycle: Lifecycle, version: u64) -> Project {
        let created_at = Utc.with_ymd_and_hms(2026, 9, 19, 12, 0, 0).unwrap();
        Project::new(
            ProjectId::from_uuid(Uuid::from_u128(0x1234)),
            "Authority".to_owned(),
            lifecycle,
            Version::try_from_value(version).unwrap(),
            created_at,
            created_at + chrono::Duration::seconds(i64::try_from(version).unwrap()),
        )
        .unwrap()
    }

    #[test]
    fn an_empty_file_becomes_a_migrated_store() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/remindr.sqlite");
        assert!(
            migration_status(&path)
                .unwrap()
                .iter()
                .all(|state| !state.applied)
        );
        assert!(migrate(&path).unwrap().iter().all(|state| state.applied));
        // Reapplying is idempotent
        assert!(migrate(&path).unwrap().iter().all(|state| state.applied));
        assert!(path.is_file());
    }

    #[test]
    fn the_journal_is_wal_and_foreign_keys_are_enforced() {
        let (_directory, store) = scratch();
        let connection = store.conn().unwrap();
        let mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert!(mode.eq_ignore_ascii_case(JOURNAL_MODE_WAL));
        let enforced: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(enforced, 1);
    }

    #[test]
    fn a_ledger_naming_an_unknown_migration_is_refused() {
        let (_directory, store) = scratch();
        store
            .conn()
            .unwrap()
            .execute(
                &format!(
                    "INSERT INTO {LEDGER} (version, name, checksum, applied_at) \
                     VALUES (99, 'future_schema', 'unknown', '1970-01-01T00:00:00.000000Z')"
                ),
                [],
            )
            .unwrap();
        assert!(matches!(
            migration_status(store.path()),
            Err(StorageError::UnknownMigration { version: 99, .. })
        ));
        assert!(matches!(
            migrate(store.path()),
            Err(StorageError::UnknownMigration { version: 99, .. })
        ));
    }

    #[test]
    fn a_rewritten_migration_is_refused_rather_than_skipped() {
        let (_directory, store) = scratch();
        store
            .conn()
            .unwrap()
            .execute(
                &format!("UPDATE {LEDGER} SET checksum = 'rewritten' WHERE version = 1"),
                [],
            )
            .unwrap();
        assert_eq!(
            migration_status(store.path()),
            Err(StorageError::MigrationChecksumDrift { version: 1 })
        );
    }

    #[test]
    fn a_recorded_migration_whose_table_vanished_is_drift() {
        let (_directory, store) = scratch();
        store
            .conn()
            .unwrap()
            .execute_batch("DROP TABLE todo_reminder_deliveries;")
            .unwrap();
        assert_eq!(
            migration_status(store.path()),
            Err(StorageError::MigrationSchemaDrift {
                version: 1,
                table: "todo_reminder_deliveries"
            })
        );
    }

    #[test]
    fn a_table_no_ledger_row_explains_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("remindr.sqlite");
        let store = Store::attach(&path).unwrap();
        store
            .conn()
            .unwrap()
            .execute_batch("CREATE TABLE projects (id TEXT PRIMARY KEY);")
            .unwrap();
        assert_eq!(
            migration_status(&path),
            Err(StorageError::UnledgeredMigrationTable { table: "projects" })
        );
        assert_eq!(
            migrate(&path),
            Err(StorageError::UnledgeredMigrationTable { table: "projects" })
        );
    }

    #[test]
    fn optimistic_project_writes_keep_one_history() {
        let (_directory, store) = scratch();
        let repository = ProjectRepository::new(store);
        let original = project(Lifecycle::Open, 1);
        repository.create(&original).unwrap();
        assert!(matches!(
            repository.create(&original),
            Err(StorageError::ProjectAlreadyExists { .. })
        ));
        assert_eq!(
            repository.find(original.id()).unwrap(),
            Some(original.clone())
        );
        assert_eq!(repository.list().unwrap(), vec![original.clone()]);

        let completed = project(Lifecycle::Completed, 2);
        repository.replace(Version::new(), &completed).unwrap();
        assert_eq!(
            repository.find(completed.id()).unwrap(),
            Some(completed.clone())
        );
        // The version that was already consumed cannot be replayed
        assert_eq!(
            repository.replace(Version::new(), &completed),
            Err(StorageError::VersionConflict {
                project_id: completed.id(),
                expected: 1,
                actual: 2
            })
        );
    }

    #[test]
    fn a_replacement_may_not_rewrite_history() {
        let (_directory, store) = scratch();
        let repository = ProjectRepository::new(store);
        let original = project(Lifecycle::Open, 1);
        repository.create(&original).unwrap();
        let backward = Project::new(
            original.id(),
            "Backward timestamp".to_owned(),
            Lifecycle::Open,
            original.version().next().unwrap(),
            original.created_at(),
            original.updated_at() - chrono::Duration::seconds(1),
        )
        .unwrap();
        assert_eq!(
            repository.replace(original.version(), &backward),
            Err(StorageError::InvalidReplacement {
                reason: "updated_at must not move backward"
            })
        );
        assert_eq!(repository.find(original.id()).unwrap(), Some(original));
    }

    #[test]
    fn replacing_an_absent_project_says_so() {
        let (_directory, store) = scratch();
        let repository = ProjectRepository::new(store);
        let absent = project(Lifecycle::Open, 2);
        assert_eq!(
            repository.replace(Version::new(), &absent),
            Err(StorageError::ProjectNotFound {
                project_id: absent.id()
            })
        );
        assert_eq!(repository.find(absent.id()).unwrap(), None);
    }

    #[test]
    fn storage_errors_do_not_echo_driver_or_connection_material() {
        let error = StorageError::Connect;
        assert_eq!(error.to_string(), "mg-remindr database connection failed");
        assert!(!format!("{error:?}").contains("sqlite"));
    }

    #[test]
    fn the_stored_timestamp_contract_rejects_sub_microseconds() {
        let exact = Utc
            .with_ymd_and_hms(2026, 8, 28, 12, 0, 0)
            .unwrap()
            .with_nanosecond(123_456_000)
            .unwrap();
        assert_eq!(validate_timestamp_precision(exact, "created_at"), Ok(()));
        assert_eq!(timestamp_text(exact), "2026-08-28T12:00:00.123456Z");
        assert_eq!(parse_timestamp(&timestamp_text(exact)).unwrap(), exact);

        let inexact = exact + chrono::Duration::nanoseconds(1);
        assert_eq!(
            validate_timestamp_precision(inexact, "created_at"),
            Err(StorageError::InvalidTimestampPrecision {
                field: "created_at"
            })
        );
    }

    #[test]
    fn stored_instants_sort_as_text_the_way_they_sort_as_time() {
        let earlier = Utc.with_ymd_and_hms(2026, 8, 28, 12, 0, 0).unwrap();
        let later = earlier + chrono::Duration::microseconds(1);
        assert!(timestamp_text(earlier) < timestamp_text(later));
    }

    #[test]
    fn tag_creation_version_overflow_has_creation_error_contract() {
        let version = Version::try_from_value(u64::MAX).unwrap();
        let error = tag_creation_database_version(version).unwrap_err();
        assert_eq!(
            error,
            StorageError::InvalidTagCreation {
                reason: "version exceeds the stored integer range"
            }
        );
        assert_eq!(
            error.to_string(),
            "invalid tag creation: version exceeds the stored integer range"
        );
    }

    #[test]
    fn the_embedded_migration_matches_its_recorded_checksum() {
        assert_eq!(validate_migration_sources(), Ok(()));
    }
}
