use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use std::{fmt, str::FromStr};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("invalid {kind} identifier")]
    InvalidId { kind: &'static str },
    #[error("{field} must not be empty or contain control characters")]
    InvalidText { field: &'static str },
    #[error("version must be at least 1")]
    InvalidVersion,
    #[error("version overflow")]
    VersionOverflow,
    #[error("invalid lifecycle transition")]
    InvalidLifecycle,
}
macro_rules! id {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);
        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
            pub const fn from_uuid(v: Uuid) -> Self {
                Self(v)
            }
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
        impl FromStr for $name {
            type Err = DomainError;
            fn from_str(v: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(v)
                    .map(Self)
                    .map_err(|_| DomainError::InvalidId { kind: $kind })
            }
        }
    };
}
id!(TodoId, "todo");
id!(ProjectId, "project");
id!(TagId, "tag");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lifecycle {
    Open,
    Completed,
    Trashed,
}
impl Lifecycle {
    pub fn transition(self, next: Self) -> Result<Self, DomainError> {
        match (self, next) {
            (Self::Open, Self::Completed | Self::Trashed)
            | (Self::Completed, Self::Open)
            | (Self::Trashed, Self::Open) => Ok(next),
            (a, b) if a == b => Ok(next),
            _ => Err(DomainError::InvalidLifecycle),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Version(u64);
impl Version {
    pub fn new() -> Self {
        Self(1)
    }
    pub fn try_from_value(v: u64) -> Result<Self, DomainError> {
        (v != 0)
            .then_some(Self(v))
            .ok_or(DomainError::InvalidVersion)
    }
    pub const fn value(self) -> u64 {
        self.0
    }
    pub fn next(self) -> Result<Self, DomainError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(DomainError::VersionOverflow)
    }
}
impl Default for Version {
    fn default() -> Self {
        Self::new()
    }
}
impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::try_from_value(u64::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
fn valid_text(value: &str, field: &'static str) -> Result<String, DomainError> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        Err(DomainError::InvalidText { field })
    } else {
        Ok(value.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Project {
    id: ProjectId,
    name: String,
    lifecycle: Lifecycle,
    version: Version,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Tag {
    id: TagId,
    name: String,
    version: Version,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Todo {
    id: TodoId,
    title: String,
    project_id: Option<ProjectId>,
    parent_id: Option<TodoId>,
    tag_ids: Vec<TagId>,
    dependency_ids: Vec<TodoId>,
    lifecycle: Lifecycle,
    version: Version,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct ProjectWire {
    id: ProjectId,
    name: String,
    lifecycle: Lifecycle,
    version: Version,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
#[derive(Deserialize)]
struct TagWire {
    id: TagId,
    name: String,
    version: Version,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
#[derive(Deserialize)]
struct TodoWire {
    id: TodoId,
    title: String,
    project_id: Option<ProjectId>,
    parent_id: Option<TodoId>,
    tag_ids: Vec<TagId>,
    dependency_ids: Vec<TodoId>,
    lifecycle: Lifecycle,
    version: Version,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
impl<'de> Deserialize<'de> for Project {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = ProjectWire::deserialize(d)?;
        Self::new(
            w.id,
            w.name,
            w.lifecycle,
            w.version,
            w.created_at,
            w.updated_at,
        )
        .map_err(serde::de::Error::custom)
    }
}
impl<'de> Deserialize<'de> for Tag {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = TagWire::deserialize(d)?;
        Self::new(w.id, w.name, w.version, w.created_at, w.updated_at)
            .map_err(serde::de::Error::custom)
    }
}
impl<'de> Deserialize<'de> for Todo {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = TodoWire::deserialize(d)?;
        Self::new(
            w.id,
            w.title,
            w.project_id,
            w.parent_id,
            w.tag_ids,
            w.dependency_ids,
            w.lifecycle,
            w.version,
            w.created_at,
            w.updated_at,
        )
        .map_err(serde::de::Error::custom)
    }
}
impl Project {
    pub fn new(
        id: ProjectId,
        name: String,
        lifecycle: Lifecycle,
        version: Version,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            id,
            name: valid_text(&name, "project name")?,
            lifecycle,
            version,
            created_at,
            updated_at,
        })
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}
impl Tag {
    pub fn new(
        id: TagId,
        name: String,
        version: Version,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            id,
            name: valid_text(&name, "tag name")?,
            version,
            created_at,
            updated_at,
        })
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}
impl Todo {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: TodoId,
        title: String,
        project_id: Option<ProjectId>,
        parent_id: Option<TodoId>,
        tag_ids: Vec<TagId>,
        dependency_ids: Vec<TodoId>,
        lifecycle: Lifecycle,
        version: Version,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            id,
            title: valid_text(&title, "todo title")?,
            project_id,
            parent_id,
            tag_ids,
            dependency_ids,
            lifecycle,
            version,
            created_at,
            updated_at,
        })
    }
    pub fn title(&self) -> &str {
        &self.title
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoRelationship {
    pub todo_id: TodoId,
    pub related_id: TodoId,
    pub kind: TodoRelationshipKind,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TodoRelationshipKind {
    Parent,
    Dependency,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagRelationship {
    pub todo_id: TodoId,
    pub tag_id: TagId,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_validate_and_check_overflow() {
        assert!(Version::try_from_value(0).is_err());
        assert_eq!(Version::new().next().unwrap().value(), 2);
        assert!(Version(u64::MAX).next().is_err());
    }
    #[test]
    fn serde_revalidates_domain_text() {
        let id = TodoId::new();
        let json = format!(
            r#"{{"id":"{id}","title":"\n","project_id":null,"parent_id":null,"tag_ids":[],"dependency_ids":[],"lifecycle":"open","version":1,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}}"#
        );
        assert!(serde_json::from_str::<Todo>(&json).is_err());
    }
}
