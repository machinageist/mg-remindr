// Author: Jeff
// Date: 2026-09-20
// Description: The one-shot path off the retired PostgreSQL database — its rows, as they stand
// Notes: The interop envelope cannot carry them: a todo at version three is neither a new todo
//        nor a version-matched replacement, and importing it would restart its history. This
//        takes the tables verbatim, checks every row before writing any, and only ever writes
//        into a store that is still empty, so a refusal leaves nothing half-adopted.
//        Delete this module once the PostgreSQL databases are gone.

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use serde::Deserialize;
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{
    DomainError, Lifecycle, Project, ProjectId, Tag, TagId, Todo, TodoDue, TodoId, Version,
};
use crate::storage::{ProjectRepository, StorageError, Store, TagRepository, TodoRepository};

#[derive(Debug, Error)]
pub enum AdoptError {
    #[error("the export is not readable as PostgreSQL rows")]
    Unreadable,
    #[error("a {kind} row is not valid: {reason}")]
    InvalidRow {
        kind: &'static str,
        reason: DomainError,
    },
    #[error("a {kind} row names an identifier that is not a UUID")]
    InvalidId { kind: &'static str },
    #[error("a todo row's due value is not one consistent form")]
    InvalidDue,
    #[error("the export carries {table} rows, which this path does not adopt")]
    UnsupportedTable { table: &'static str },
    #[error("the store already holds reminders; adoption only fills an empty store")]
    StoreNotEmpty,
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// What one adoption carried across.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adoption {
    pub projects: usize,
    pub tags: usize,
    pub todos: usize,
    pub revision: u64,
}

// ── The shape `psql` is asked to emit: the tables, column for column ──

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostgresExport {
    #[serde(default)]
    projects: Vec<ProjectRow>,
    #[serde(default)]
    tags: Vec<TagRow>,
    #[serde(default)]
    todos: Vec<TodoRow>,
    /// The checkpoint consumers remember, carried so none of them is handed a lower one
    #[serde(default)]
    authority_revision: u64,
    // The tables this path does not adopt. They are named rather than ignored, so an
    // export that holds them is refused instead of quietly losing them.
    #[serde(default)]
    todo_tags: Vec<serde_json::Value>,
    #[serde(default)]
    todo_parents: Vec<serde_json::Value>,
    #[serde(default)]
    todo_dependencies: Vec<serde_json::Value>,
    #[serde(default)]
    todo_recurrence: Vec<serde_json::Value>,
    #[serde(default)]
    todo_reminders: Vec<serde_json::Value>,
    #[serde(default)]
    todo_reminder_deliveries: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRow {
    id: String,
    name: String,
    lifecycle: String,
    version: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TagRow {
    id: String,
    name: String,
    version: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoRow {
    id: String,
    title: String,
    project_id: Option<String>,
    lifecycle: String,
    version: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    trashed_at: Option<DateTime<Utc>>,
    due_date: Option<NaiveDate>,
    due_at: Option<DateTime<Utc>>,
    due_timezone: Option<String>,
}

// Adopt every row the export carries, or none of them
///
/// # Errors
/// Returns an error when the export is unreadable, holds a row the domain refuses, carries a
/// table this path does not adopt, or the store already holds reminders.
pub fn adopt_postgres_rows(store: &Store, document: &str) -> Result<Adoption, AdoptError> {
    let export: PostgresExport =
        serde_json::from_str(document).map_err(|_| AdoptError::Unreadable)?;
    refuse_unsupported_tables(&export)?;

    // every row becomes a domain object first, so a bad export is refused before any write
    let projects = export
        .projects
        .iter()
        .map(project)
        .collect::<Result<Vec<_>, _>>()?;
    let tags = export.tags.iter().map(tag).collect::<Result<Vec<_>, _>>()?;
    let todos = export
        .todos
        .iter()
        .map(todo)
        .collect::<Result<Vec<_>, _>>()?;

    let project_repository = ProjectRepository::new(store.clone());
    let tag_repository = TagRepository::new(store.clone());
    let todo_repository = TodoRepository::new(store.clone());
    if !project_repository.list()?.is_empty()
        || !tag_repository.list()?.is_empty()
        || !todo_repository.list()?.is_empty()
    {
        return Err(AdoptError::StoreNotEmpty);
    }

    // projects and tags first: a todo may name either of them
    for project in &projects {
        project_repository.create(project)?;
    }
    for tag in &tags {
        tag_repository.create(tag)?;
    }
    for todo in &todos {
        todo_repository.create(todo)?;
    }
    let revision = crate::storage::raise_authority_revision(store, export.authority_revision)?;

    Ok(Adoption {
        projects: projects.len(),
        tags: tags.len(),
        todos: todos.len(),
        revision,
    })
}

// Refuse an export holding a table this path cannot carry, naming the first one found
fn refuse_unsupported_tables(export: &PostgresExport) -> Result<(), AdoptError> {
    for (table, rows) in [
        ("todo_tags", export.todo_tags.len()),
        ("todo_parents", export.todo_parents.len()),
        ("todo_dependencies", export.todo_dependencies.len()),
        ("todo_recurrence", export.todo_recurrence.len()),
        ("todo_reminders", export.todo_reminders.len()),
        (
            "todo_reminder_deliveries",
            export.todo_reminder_deliveries.len(),
        ),
    ] {
        if rows > 0 {
            return Err(AdoptError::UnsupportedTable { table });
        }
    }
    Ok(())
}

// ── One row, one domain object ──

fn project(row: &ProjectRow) -> Result<Project, AdoptError> {
    Project::new(
        ProjectId::from_uuid(uuid(&row.id, "project")?),
        row.name.clone(),
        lifecycle(&row.lifecycle, "project")?,
        version(row.version, "project")?,
        row.created_at,
        row.updated_at,
    )
    .map_err(|reason| AdoptError::InvalidRow {
        kind: "project",
        reason,
    })
}

fn tag(row: &TagRow) -> Result<Tag, AdoptError> {
    Tag::new(
        TagId::from_uuid(uuid(&row.id, "tag")?),
        row.name.clone(),
        version(row.version, "tag")?,
        row.created_at,
        row.updated_at,
    )
    .map_err(|reason| AdoptError::InvalidRow {
        kind: "tag",
        reason,
    })
}

fn todo(row: &TodoRow) -> Result<Todo, AdoptError> {
    let project_id = row
        .project_id
        .as_deref()
        .map(|id| uuid(id, "todo").map(ProjectId::from_uuid))
        .transpose()?;
    Todo::new(
        TodoId::from_uuid(uuid(&row.id, "todo")?),
        row.title.clone(),
        project_id,
        None,
        Vec::new(),
        Vec::new(),
        lifecycle(&row.lifecycle, "todo")?,
        version(row.version, "todo")?,
        row.created_at,
        row.updated_at,
        row.completed_at,
        row.trashed_at,
        due(row)?,
    )
    .map_err(|reason| AdoptError::InvalidRow {
        kind: "todo",
        reason,
    })
}

// The three nullable due columns are one value: absent, an all-day date, or a zoned instant
fn due(row: &TodoRow) -> Result<Option<TodoDue>, AdoptError> {
    match (row.due_date, row.due_at, row.due_timezone.as_deref()) {
        (None, None, None) => Ok(None),
        (Some(date), None, Some(timezone)) => TodoDue::date(date, timezone.to_owned())
            .map(Some)
            .map_err(|_| AdoptError::InvalidDue),
        (None, Some(at), Some(timezone)) => {
            let zone = timezone.parse::<Tz>().map_err(|_| AdoptError::InvalidDue)?;
            TodoDue::timed(at.with_timezone(&zone).fixed_offset(), timezone.to_owned())
                .map(Some)
                .map_err(|_| AdoptError::InvalidDue)
        }
        _ => Err(AdoptError::InvalidDue),
    }
}

fn uuid(value: &str, kind: &'static str) -> Result<Uuid, AdoptError> {
    value
        .parse::<Uuid>()
        .map_err(|_| AdoptError::InvalidId { kind })
}

fn lifecycle(value: &str, kind: &'static str) -> Result<Lifecycle, AdoptError> {
    match value {
        "open" => Ok(Lifecycle::Open),
        "completed" => Ok(Lifecycle::Completed),
        "trashed" => Ok(Lifecycle::Trashed),
        _ => Err(AdoptError::InvalidRow {
            kind,
            reason: DomainError::InvalidLifecycle,
        }),
    }
}

fn version(value: u64, kind: &'static str) -> Result<Version, AdoptError> {
    Version::try_from_value(value).map_err(|reason| AdoptError::InvalidRow { kind, reason })
}
