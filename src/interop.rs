use crate::config::DatabaseUrl;
use crate::storage::{AuthorityExport, StorageError, export_authority};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

const SCHEMA: &str = "mg.interop/1";
const APP: &str = "mg-remindr";

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub interop_schema: String,
    pub kind: String,
    pub producer: Producer,
    pub export_id: String,
    pub created_at: DateTime<Utc>,
    pub source_revision: String,
    pub producer_revision: u64,
    pub completeness: Completeness,
    pub records: Vec<Record>,
    pub links: Vec<Link>,
    pub provenance: Vec<Provenance>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Serialize)]
pub struct Producer {
    pub app: String,
    pub app_version: String,
}
#[derive(Debug, Serialize)]
pub struct Completeness {
    pub complete: bool,
    pub expected_records: usize,
    pub expected_links: usize,
}
#[derive(Debug, Serialize)]
pub struct Record {
    pub global_id: String,
    pub origin: Origin,
    pub revision: i64,
    pub observed_at: DateTime<Utc>,
    pub lifecycle: Lifecycle,
    pub payload: serde_json::Value,
}
#[derive(Debug, Serialize)]
pub struct Origin {
    pub app: String,
    pub kind: String,
    pub local_id: String,
}
#[derive(Debug, Serialize)]
pub struct Lifecycle {
    pub state: String,
    pub deleted_at: Option<DateTime<Utc>>,
    pub tombstoned_at: Option<DateTime<Utc>>,
    pub trashed_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    pub purged: bool,
}
#[derive(Debug, Serialize)]
pub struct Link {
    pub link_id: String,
    pub source_global_id: String,
    pub target_global_id: String,
    pub relation: String,
    pub created_by: String,
    pub created_at: Option<DateTime<Utc>>,
    pub provenance: String,
}
#[derive(Debug, Serialize)]
pub struct Provenance {
    pub source: String,
    pub boundary: String,
}
#[derive(Debug, Serialize)]
pub struct Diagnostic {
    pub severity: String,
    pub code: String,
    pub message: String,
}

pub async fn export(database_url: &DatabaseUrl) -> Result<Snapshot, StorageError> {
    let AuthorityExport {
        projects,
        tags,
        todos,
        revision,
    } = export_authority(database_url).await?;
    let mut records = Vec::with_capacity(projects.len() + tags.len() + todos.len());
    let mut links = Vec::new();
    for project in projects {
        let id = project.id().to_string();
        records.push(Record {
            global_id: format!("{APP}:project:{id}"),
            origin: Origin {
                app: APP.into(),
                kind: "project".into(),
                local_id: id,
            },
            revision: i64::try_from(project.version().value()).map_err(|_| {
                StorageError::Database {
                    operation: "interop export",
                }
            })?,
            observed_at: project.updated_at(),
            lifecycle: lifecycle(project.lifecycle(), None),
            payload: serde_json::to_value(project).map_err(|_| StorageError::Database {
                operation: "interop export",
            })?,
        });
    }
    for tag in tags {
        let id = tag.id().to_string();
        records.push(Record {
            global_id: format!("{APP}:tag:{id}"),
            origin: Origin {
                app: APP.into(),
                kind: "tag".into(),
                local_id: id,
            },
            revision: i64::try_from(tag.version().value()).map_err(|_| StorageError::Database {
                operation: "interop export",
            })?,
            observed_at: tag.updated_at(),
            lifecycle: Lifecycle {
                state: "active".into(),
                deleted_at: None,
                tombstoned_at: None,
                trashed_at: None,
                archived_at: None,
                purged: false,
            },
            payload: serde_json::to_value(tag).map_err(|_| StorageError::Database {
                operation: "interop export",
            })?,
        });
    }
    for todo in todos {
        validate_exportable_todo(&todo)?;
        let id = todo.id().to_string();
        let todo_global = format!("{APP}:todo:{id}");
        if let Some(project_id) = todo.project_id() {
            links.push(link(
                &format!("{APP}:project:{project_id}"),
                &todo_global,
                "project_contains_todo",
            ));
        }
        let parent = todo.parent_id();
        if let Some(parent_id) = parent {
            links.push(link(
                &format!("{APP}:todo:{parent_id}"),
                &todo_global,
                "todo_parent",
            ));
        }
        for dependency_id in todo.dependency_ids() {
            links.push(link(
                &todo_global,
                &format!("{APP}:todo:{dependency_id}"),
                "todo_depends_on",
            ));
        }
        for tag_id in todo.tag_ids() {
            links.push(link(
                &todo_global,
                &format!("{APP}:tag:{tag_id}"),
                "todo_tagged",
            ));
        }
        records.push(Record {
            global_id: todo_global,
            origin: Origin {
                app: APP.into(),
                kind: "todo".into(),
                local_id: id,
            },
            revision: i64::try_from(todo.version().value()).map_err(|_| {
                StorageError::Database {
                    operation: "interop export",
                }
            })?,
            observed_at: todo.updated_at(),
            lifecycle: lifecycle(todo.lifecycle(), todo.trashed_at()),
            payload: todo_payload(&todo),
        });
    }
    records.sort_by(|a, b| a.global_id.cmp(&b.global_id));
    links.sort_by(|a, b| a.link_id.cmp(&b.link_id));
    let created_at = records
        .iter()
        .map(|r| r.observed_at)
        .max()
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let identity = serde_json::json!({ "interop_schema": SCHEMA, "kind": "snapshot", "producer": {"app": APP, "app_version": env!("CARGO_PKG_VERSION")}, "created_at": created_at, "records": &records, "links": &links });
    let digest = digest(
        &serde_json::to_vec(&identity).map_err(|_| StorageError::Database {
            operation: "interop export",
        })?,
    );
    Ok(Snapshot {
        interop_schema: SCHEMA.into(),
        kind: "snapshot".into(),
        producer: Producer {
            app: APP.into(),
            app_version: env!("CARGO_PKG_VERSION").into(),
        },
        export_id: format!("{APP}:snapshot:{digest}"),
        created_at,
        source_revision: digest,
        producer_revision: revision,
        completeness: Completeness {
            complete: true,
            expected_records: records.len(),
            expected_links: links.len(),
        },
        records,
        links,
        provenance: vec![Provenance {
            source: APP.into(),
            boundary: "authoritative read-only PostgreSQL repeatable-read transaction".into(),
        }],
        diagnostics: vec![Diagnostic {
            severity: "info".into(),
            code: "purged_absence".into(),
            message: "Purged rows are absent from the authority export.".into(),
        }],
    })
}

fn todo_payload(todo: &crate::domain::Todo) -> serde_json::Value {
    serde_json::json!({
        "id": todo.id(),
        "title": todo.title(),
        "due": todo.due(),
        "recurrence": null,
        "reminders": [],
        "priority": "none",
        "project_id": todo.project_id(),
        "tag_ids": todo.tag_ids(),
        "dependency_ids": todo.dependency_ids(),
        "notes": null,
        "parent_id": todo.parent_id(),
        "completed_at": todo.completed_at(),
        "trashed_at": todo.trashed_at(),
        "version": todo.version().value(),
        "created_at": todo.created_at(),
        "updated_at": todo.updated_at()
    })
}

// Every lifecycle is representable once its transition time is stored alongside it
fn validate_exportable_todo(todo: &crate::domain::Todo) -> Result<(), StorageError> {
    let recorded = match todo.lifecycle() {
        crate::domain::Lifecycle::Open => true,
        crate::domain::Lifecycle::Completed => todo.completed_at().is_some(),
        crate::domain::Lifecycle::Trashed => todo.trashed_at().is_some(),
    };
    if !recorded {
        return Err(StorageError::UnrepresentableAuthority {
            kind: "todo lifecycle",
        });
    }
    Ok(())
}

fn lifecycle(value: crate::domain::Lifecycle, trashed_at: Option<DateTime<Utc>>) -> Lifecycle {
    let (state, trashed_at) = match value {
        crate::domain::Lifecycle::Open | crate::domain::Lifecycle::Completed => ("active", None),
        crate::domain::Lifecycle::Trashed => ("trashed", trashed_at),
    };
    Lifecycle {
        state: state.into(),
        deleted_at: None,
        tombstoned_at: None,
        trashed_at,
        archived_at: None,
        purged: false,
    }
}
fn link(source: &str, target: &str, relation: &str) -> Link {
    Link {
        link_id: format!("{source}--{relation}--{target}"),
        source_global_id: source.into(),
        target_global_id: target.into(),
        relation: relation.into(),
        created_by: APP.into(),
        created_at: None,
        provenance: "mg-remindr authoritative relationship".into(),
    }
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Lifecycle as TodoLifecycle, Todo, TodoId, Version};
    use chrono::TimeZone;

    #[test]
    fn recorded_lifecycles_export_and_carry_their_transition_time() {
        let timestamp = Utc.with_ymd_and_hms(2026, 9, 2, 12, 0, 0).unwrap();
        let build = |lifecycle, completed_at, trashed_at| {
            Todo::new(
                TodoId::new(),
                "completed work".to_owned(),
                None,
                None,
                Vec::new(),
                Vec::new(),
                lifecycle,
                Version::new(),
                timestamp,
                timestamp,
                completed_at,
                trashed_at,
                None,
            )
            .unwrap()
        };

        let completed = build(TodoLifecycle::Completed, Some(timestamp), None);
        assert_eq!(validate_exportable_todo(&completed), Ok(()));
        let payload = todo_payload(&completed);
        assert_eq!(payload["completed_at"], serde_json::json!(timestamp));
        assert!(payload["trashed_at"].is_null());
        let state = lifecycle(completed.lifecycle(), completed.trashed_at());
        assert_eq!(state.state, "active");
        assert_eq!(state.trashed_at, None);

        let trashed = build(TodoLifecycle::Trashed, None, Some(timestamp));
        assert_eq!(validate_exportable_todo(&trashed), Ok(()));
        let payload = todo_payload(&trashed);
        assert!(payload["completed_at"].is_null());
        assert_eq!(payload["trashed_at"], serde_json::json!(timestamp));
        // mg-calr rejects a snapshot whose record lifecycle and payload disagree
        let state = lifecycle(trashed.lifecycle(), trashed.trashed_at());
        assert_eq!(state.state, "trashed");
        assert_eq!(state.trashed_at, Some(timestamp));
    }

    #[test]
    fn todo_payload_is_the_lossless_calr_mvp_shape() {
        let timestamp = Utc.with_ymd_and_hms(2026, 9, 2, 12, 0, 0).unwrap();
        let todo = Todo::new(
            TodoId::new(),
            "ship snapshot".to_owned(),
            None,
            None,
            Vec::new(),
            Vec::new(),
            TodoLifecycle::Open,
            Version::new(),
            timestamp,
            timestamp,
            None,
            None,
            None,
        )
        .unwrap();
        let payload = todo_payload(&todo);
        let object = payload.as_object().unwrap();
        let expected = [
            "id",
            "title",
            "due",
            "recurrence",
            "reminders",
            "priority",
            "project_id",
            "tag_ids",
            "dependency_ids",
            "notes",
            "parent_id",
            "completed_at",
            "trashed_at",
            "version",
            "created_at",
            "updated_at",
        ];
        assert_eq!(object.len(), expected.len());
        for key in expected {
            assert!(object.contains_key(key), "missing payload key: {key}");
        }
        assert!(object["due"].is_null());
        assert!(object["recurrence"].is_null());
        assert_eq!(object["reminders"], serde_json::json!([]));
        assert_eq!(object["priority"], "none");
        assert_eq!(object["version"], 1);
    }
}
