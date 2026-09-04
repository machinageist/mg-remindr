use chrono::{DateTime, FixedOffset, NaiveDate, Offset, Utc};
use chrono_tz::Tz;
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
    #[error("updated_at must not be earlier than created_at")]
    InvalidTimestamps,
    #[error("{lifecycle} lifecycle requires its transition time")]
    MissingTransitionTime { lifecycle: &'static str },
    #[error("{lifecycle} lifecycle must not carry a {field}")]
    UnexpectedTransitionTime {
        lifecycle: &'static str,
        field: &'static str,
    },
    #[error("{field} must fall between created_at and updated_at")]
    TransitionTimeOutsideHistory { field: &'static str },
    #[error("due timezone must be a named IANA zone")]
    InvalidDueTimezone,
    #[error("due offset does not match its IANA timezone at that instant")]
    DueOffsetTimezoneMismatch,
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
    pub const fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Completed => "completed",
            Self::Trashed => "trashed",
        }
    }
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
/// A due value in the externally tagged shape the mg-calr projection consumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TodoDue {
    Date {
        date: NaiveDate,
        timezone: String,
    },
    Timed {
        at: DateTime<FixedOffset>,
        timezone: String,
    },
}

impl TodoDue {
    /// Build an all-day due value in a named zone.
    pub fn date(date: NaiveDate, timezone: String) -> Result<Self, DomainError> {
        let timezone = named_zone(timezone)?.0;
        Ok(Self::Date { date, timezone })
    }
    /// Build a timed due value whose offset must agree with its zone at that instant.
    pub fn timed(at: DateTime<FixedOffset>, timezone: String) -> Result<Self, DomainError> {
        let (timezone, zone) = named_zone(timezone)?;
        if at.with_timezone(&zone).offset().fix() != *at.offset() {
            return Err(DomainError::DueOffsetTimezoneMismatch);
        }
        Ok(Self::Timed { at, timezone })
    }
    /// Revalidate a due value that arrived from storage or the wire.
    pub fn revalidated(self) -> Result<Self, DomainError> {
        match self {
            Self::Date { date, timezone } => Self::date(date, timezone),
            Self::Timed { at, timezone } => Self::timed(at, timezone),
        }
    }
    pub fn timezone(&self) -> &str {
        match self {
            Self::Date { timezone, .. } | Self::Timed { timezone, .. } => timezone,
        }
    }
}

fn named_zone(timezone: String) -> Result<(String, Tz), DomainError> {
    let zone = timezone
        .parse::<Tz>()
        .map_err(|_| DomainError::InvalidDueTimezone)?;
    Ok((timezone, zone))
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
    completed_at: Option<DateTime<Utc>>,
    trashed_at: Option<DateTime<Utc>>,
    due: Option<TodoDue>,
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
    #[serde(default)]
    completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    trashed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    due: Option<TodoDue>,
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
            w.completed_at,
            w.trashed_at,
            w.due,
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
        if updated_at < created_at {
            return Err(DomainError::InvalidTimestamps);
        }
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
    pub const fn id(&self) -> ProjectId {
        self.id
    }
    pub const fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }
    pub const fn version(&self) -> Version {
        self.version
    }
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub const fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
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
        if updated_at < created_at {
            return Err(DomainError::InvalidTimestamps);
        }
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
    pub const fn id(&self) -> TagId {
        self.id
    }
    pub const fn version(&self) -> Version {
        self.version
    }
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub const fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
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
        completed_at: Option<DateTime<Utc>>,
        trashed_at: Option<DateTime<Utc>>,
        due: Option<TodoDue>,
    ) -> Result<Self, DomainError> {
        if updated_at < created_at {
            return Err(DomainError::InvalidTimestamps);
        }
        validate_transition_times(lifecycle, completed_at, trashed_at, created_at, updated_at)?;
        let due = due.map(TodoDue::revalidated).transpose()?;
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
            completed_at,
            trashed_at,
            due,
        })
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub const fn id(&self) -> TodoId {
        self.id
    }
    pub const fn project_id(&self) -> Option<ProjectId> {
        self.project_id
    }
    pub const fn parent_id(&self) -> Option<TodoId> {
        self.parent_id
    }
    pub fn tag_ids(&self) -> &[TagId] {
        &self.tag_ids
    }
    pub fn dependency_ids(&self) -> &[TodoId] {
        &self.dependency_ids
    }
    pub const fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }
    pub const fn version(&self) -> Version {
        self.version
    }
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub const fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
    pub const fn completed_at(&self) -> Option<DateTime<Utc>> {
        self.completed_at
    }
    pub const fn trashed_at(&self) -> Option<DateTime<Utc>> {
        self.trashed_at
    }
    pub const fn due(&self) -> Option<&TodoDue> {
        self.due.as_ref()
    }
}

// A lifecycle carries exactly the transition time it earned, inside the row's own history
fn validate_transition_times(
    lifecycle: Lifecycle,
    completed_at: Option<DateTime<Utc>>,
    trashed_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
) -> Result<(), DomainError> {
    let expected = match lifecycle {
        Lifecycle::Open => (false, false),
        Lifecycle::Completed => (true, false),
        Lifecycle::Trashed => (false, true),
    };
    for (present, wanted, field) in [
        (completed_at.is_some(), expected.0, "completed_at"),
        (trashed_at.is_some(), expected.1, "trashed_at"),
    ] {
        match (present, wanted) {
            (false, true) => {
                return Err(DomainError::MissingTransitionTime {
                    lifecycle: lifecycle.label(),
                });
            }
            (true, false) => {
                return Err(DomainError::UnexpectedTransitionTime {
                    lifecycle: lifecycle.label(),
                    field,
                });
            }
            _ => {}
        }
    }
    for (value, field) in [(completed_at, "completed_at"), (trashed_at, "trashed_at")] {
        if let Some(value) = value
            && (value < created_at || value > updated_at)
        {
            return Err(DomainError::TransitionTimeOutsideHistory { field });
        }
    }
    Ok(())
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
    #[test]
    fn tags_reject_invalid_text_and_timestamp_history() {
        let created_at = "2026-01-02T00:00:00Z".parse().unwrap();
        let earlier = "2026-01-01T00:00:00Z".parse().unwrap();
        assert!(matches!(
            Tag::new(
                TagId::new(),
                "\n".to_owned(),
                Version::new(),
                created_at,
                created_at
            ),
            Err(DomainError::InvalidText { field: "tag name" })
        ));
        assert_eq!(
            Tag::new(
                TagId::new(),
                "tag".to_owned(),
                Version::new(),
                created_at,
                earlier,
            ),
            Err(DomainError::InvalidTimestamps)
        );
    }

    #[test]
    fn projects_reject_backward_timestamp_history_in_constructor_and_serde() {
        let created_at = "2026-01-02T00:00:00Z".parse().unwrap();
        let earlier = "2026-01-01T00:00:00Z".parse().unwrap();
        assert_eq!(
            Project::new(
                ProjectId::new(),
                "project".to_owned(),
                Lifecycle::Open,
                Version::new(),
                created_at,
                earlier,
            ),
            Err(DomainError::InvalidTimestamps)
        );
        let id = ProjectId::new();
        let json = format!(
            r#"{{"id":"{id}","name":"project","lifecycle":"open","version":1,"created_at":"2026-01-02T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}}"#
        );
        assert!(serde_json::from_str::<Project>(&json).is_err());
    }

    #[test]
    fn todos_reject_backward_timestamp_history_in_constructor_and_serde() {
        let created_at = "2026-01-02T00:00:00Z".parse().unwrap();
        let earlier = "2026-01-01T00:00:00Z".parse().unwrap();
        assert_eq!(
            Todo::new(
                TodoId::new(),
                "todo".to_owned(),
                None,
                None,
                vec![],
                vec![],
                Lifecycle::Open,
                Version::new(),
                created_at,
                earlier,
                None,
                None,
                None,
            ),
            Err(DomainError::InvalidTimestamps)
        );
        let id = TodoId::new();
        let json = format!(
            r#"{{"id":"{id}","title":"todo","project_id":null,"parent_id":null,"tag_ids":[],"dependency_ids":[],"lifecycle":"open","version":1,"created_at":"2026-01-02T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}}"#
        );
        assert!(serde_json::from_str::<Todo>(&json).is_err());
    }

    #[test]
    fn todo_transition_times_must_match_their_lifecycle() {
        let created_at: DateTime<Utc> = "2026-01-02T00:00:00Z".parse().unwrap();
        let updated_at: DateTime<Utc> = "2026-01-03T00:00:00Z".parse().unwrap();
        let before: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let todo = |lifecycle, completed_at, trashed_at| {
            Todo::new(
                TodoId::new(),
                "todo".to_owned(),
                None,
                None,
                vec![],
                vec![],
                lifecycle,
                Version::new(),
                created_at,
                updated_at,
                completed_at,
                trashed_at,
                None,
            )
        };
        assert!(todo(Lifecycle::Open, None, None).is_ok());
        assert!(todo(Lifecycle::Completed, Some(updated_at), None).is_ok());
        assert!(todo(Lifecycle::Trashed, None, Some(updated_at)).is_ok());
        assert_eq!(
            todo(Lifecycle::Completed, None, None),
            Err(DomainError::MissingTransitionTime {
                lifecycle: "completed"
            })
        );
        assert_eq!(
            todo(Lifecycle::Trashed, None, None),
            Err(DomainError::MissingTransitionTime {
                lifecycle: "trashed"
            })
        );
        assert_eq!(
            todo(Lifecycle::Open, Some(updated_at), None),
            Err(DomainError::UnexpectedTransitionTime {
                lifecycle: "open",
                field: "completed_at"
            })
        );
        assert_eq!(
            todo(Lifecycle::Completed, Some(updated_at), Some(updated_at)),
            Err(DomainError::UnexpectedTransitionTime {
                lifecycle: "completed",
                field: "trashed_at"
            })
        );
        assert_eq!(
            todo(Lifecycle::Completed, Some(before), None),
            Err(DomainError::TransitionTimeOutsideHistory {
                field: "completed_at"
            })
        );
        assert_eq!(
            todo(
                Lifecycle::Trashed,
                None,
                Some(updated_at + chrono::Duration::seconds(1))
            ),
            Err(DomainError::TransitionTimeOutsideHistory {
                field: "trashed_at"
            })
        );
    }

    #[test]
    fn due_values_validate_their_zone_and_offset() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 5).unwrap();
        assert!(TodoDue::date(date, "America/New_York".to_owned()).is_ok());
        assert_eq!(
            TodoDue::date(date, "Mars/Olympus".to_owned()),
            Err(DomainError::InvalidDueTimezone)
        );
        assert_eq!(
            TodoDue::date(date, String::new()),
            Err(DomainError::InvalidDueTimezone)
        );

        let eastern: DateTime<FixedOffset> = "2026-09-05T14:00:00-04:00".parse().unwrap();
        assert!(TodoDue::timed(eastern, "America/New_York".to_owned()).is_ok());
        assert_eq!(
            TodoDue::timed(eastern, "Europe/Berlin".to_owned()),
            Err(DomainError::DueOffsetTimezoneMismatch)
        );
        // Winter in New York is -05:00, so a summer offset is rejected out of season
        let winter: DateTime<FixedOffset> = "2026-01-05T14:00:00-04:00".parse().unwrap();
        assert_eq!(
            TodoDue::timed(winter, "America/New_York".to_owned()),
            Err(DomainError::DueOffsetTimezoneMismatch)
        );
    }

    #[test]
    fn todo_due_round_trips_through_the_projection_shape() {
        let json = r#"{"Date":{"date":"2026-09-05","timezone":"America/New_York"}}"#;
        let due: TodoDue = serde_json::from_str(json).unwrap();
        assert_eq!(due.timezone(), "America/New_York");
        assert_eq!(serde_json::to_string(&due).unwrap(), json);

        let id = TodoId::new();
        let with_due = format!(
            r#"{{"id":"{id}","title":"todo","project_id":null,"parent_id":null,"tag_ids":[],"dependency_ids":[],"lifecycle":"open","version":1,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","due":{json}}}"#
        );
        assert_eq!(
            serde_json::from_str::<Todo>(&with_due).unwrap().due(),
            Some(&due)
        );
        let bad_zone = with_due.replace("America/New_York", "Mars/Olympus");
        assert!(serde_json::from_str::<Todo>(&bad_zone).is_err());
    }

    #[test]
    fn todo_wire_defaults_keep_open_automation_working_and_reject_lifecycle_loss() {
        let id = TodoId::new();
        let open = format!(
            r#"{{"id":"{id}","title":"todo","project_id":null,"parent_id":null,"tag_ids":[],"dependency_ids":[],"lifecycle":"open","version":1,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}}"#
        );
        assert!(serde_json::from_str::<Todo>(&open).is_ok());
        let completed = format!(
            r#"{{"id":"{id}","title":"todo","project_id":null,"parent_id":null,"tag_ids":[],"dependency_ids":[],"lifecycle":"completed","version":1,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}}"#
        );
        assert!(serde_json::from_str::<Todo>(&completed).is_err());
        let recorded = format!(
            r#"{{"id":"{id}","title":"todo","project_id":null,"parent_id":null,"tag_ids":[],"dependency_ids":[],"lifecycle":"completed","version":1,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","completed_at":"2026-01-01T00:00:00Z"}}"#
        );
        assert_eq!(
            serde_json::from_str::<Todo>(&recorded)
                .unwrap()
                .completed_at(),
            Some("2026-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap())
        );
    }
}
