// Author: Jeff
// Date: 2026-09-04
// Description: Flag-driven reminder surface that owns identity, version, and timestamps
// Notes: JSON create/replace stays the automation surface; this is what a person types

use crate::domain::{DomainError, Lifecycle, Todo, TodoDue, TodoId};
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, SubsecRound, Utc};
use chrono_tz::Tz;
use std::{fmt, fs, path::Path};

const ZONEINFO: &str = "/usr/share/zoneinfo/";
// UUIDv7 leads with a millisecond timestamp, so the entropy a person can quote is at the tail
const HANDLE_LEN: usize = 8;
const DUE_FORMATS: [&str; 3] = ["%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S"];

#[derive(Debug, PartialEq, Eq)]
pub enum HumanError {
    UnknownZone,
    UnresolvedZone,
    UnreadableDue,
    NoMatch(String),
    Ambiguous(String, Vec<String>),
    AlreadyClosed(&'static str),
    Domain(DomainError),
}

impl fmt::Display for HumanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownZone => formatter.write_str("timezone is not a named IANA zone"),
            Self::UnresolvedZone => formatter.write_str(
                "system timezone could not be resolved; pass --timezone with an IANA zone",
            ),
            Self::UnreadableDue => {
                formatter.write_str("due must be today, tomorrow, YYYY-MM-DD, or YYYY-MM-DDTHH:MM")
            }
            Self::NoMatch(prefix) => write!(formatter, "no todo matches '{prefix}'"),
            Self::Ambiguous(prefix, matches) => write!(
                formatter,
                "'{prefix}' matches {} todos: {}",
                matches.len(),
                matches.join(", ")
            ),
            Self::AlreadyClosed(lifecycle) => write!(formatter, "todo is already {lifecycle}"),
            Self::Domain(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for HumanError {}

/// Resolve the zone a due value is written in, preferring an explicit request.
///
/// # Errors
/// Returns an error when the name is not an IANA zone or no system zone is discoverable.
pub fn resolve_zone(requested: Option<&str>) -> Result<String, HumanError> {
    let name = match requested {
        Some(name) => name.to_owned(),
        None => system_zone().ok_or(HumanError::UnresolvedZone)?,
    };
    name.parse::<Tz>().map_err(|_| HumanError::UnknownZone)?;
    Ok(name)
}

// TZ wins, then whatever zoneinfo path /etc/localtime points at
fn system_zone() -> Option<String> {
    if let Ok(name) = std::env::var("TZ")
        && !name.trim().is_empty()
    {
        return Some(name.trim().trim_start_matches(':').to_owned());
    }
    let target = fs::read_link(Path::new("/etc/localtime")).ok()?;
    let target = target.to_str()?;
    target
        .find(ZONEINFO)
        .map(|start| target[start + ZONEINFO.len()..].to_owned())
}

/// Today's civil date in one named zone.
///
/// # Errors
/// Returns an error when the name is not an IANA zone.
pub fn today_in(zone_name: &str, now: DateTime<Utc>) -> Result<NaiveDate, HumanError> {
    let zone = zone_name
        .parse::<Tz>()
        .map_err(|_| HumanError::UnknownZone)?;
    Ok(now.with_timezone(&zone).date_naive())
}

/// Read a due value written the way a person writes one.
///
/// # Errors
/// Returns an error when the zone is unknown or the value is not a form this accepts.
pub fn parse_due(value: &str, zone_name: &str, today: NaiveDate) -> Result<TodoDue, HumanError> {
    let zone = zone_name
        .parse::<Tz>()
        .map_err(|_| HumanError::UnknownZone)?;
    let trimmed = value.trim();
    let all_day = match trimmed.to_ascii_lowercase().as_str() {
        "today" => Some(today),
        "tomorrow" => today.succ_opt(),
        _ => NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").ok(),
    };
    if let Some(date) = all_day {
        return TodoDue::date(date, zone_name.to_owned()).map_err(HumanError::Domain);
    }
    let local = DUE_FORMATS
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(trimmed, format).ok())
        .ok_or(HumanError::UnreadableDue)?;
    // A local time that does not exist or repeats across a transition is not unambiguous
    let at: DateTime<FixedOffset> = local
        .and_local_timezone(zone)
        .single()
        .ok_or(HumanError::UnreadableDue)?
        .fixed_offset();
    TodoDue::timed(at, zone_name.to_owned()).map_err(HumanError::Domain)
}

/// A microsecond-precision instant, which is the precision the authority stores.
#[must_use]
pub fn now() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(6)
}

/// Build an open todo from what a person supplied.
///
/// # Errors
/// Returns an error when the title or due value fails domain validation.
pub fn new_todo(
    title: String,
    due: Option<TodoDue>,
    at: DateTime<Utc>,
) -> Result<Todo, HumanError> {
    Todo::new(
        TodoId::new(),
        title,
        None,
        None,
        Vec::new(),
        Vec::new(),
        Lifecycle::Open,
        crate::domain::Version::new(),
        at,
        at,
        None,
        None,
        due,
    )
    .map_err(HumanError::Domain)
}

/// Find the one todo a token names, refusing to guess between several.
///
/// Accepts the displayed handle, any trailing part of an identifier, or a leading
/// part including the full identifier, so both people and scripts can name a todo.
///
/// # Errors
/// Returns an error when nothing matches or more than one does.
pub fn resolve_handle(todos: &[Todo], token: &str) -> Result<TodoId, HumanError> {
    let needle = token.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Err(HumanError::NoMatch(token.to_owned()));
    }
    let matches = todos
        .iter()
        .filter(|todo| {
            let id = todo.id().to_string();
            id.ends_with(&needle) || id.starts_with(&needle)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(HumanError::NoMatch(token.to_owned())),
        [only] => Ok(only.id()),
        several => Err(HumanError::Ambiguous(
            token.to_owned(),
            several
                .iter()
                .map(|todo| format!("{} {}", handle(todo.id()), todo.title()))
                .collect(),
        )),
    }
}

/// Build the replacement that moves one open todo into a closed lifecycle.
///
/// # Errors
/// Returns an error when the todo is already closed or the version cannot advance.
pub fn close(current: &Todo, lifecycle: Lifecycle, at: DateTime<Utc>) -> Result<Todo, HumanError> {
    if current.lifecycle() != Lifecycle::Open {
        return Err(HumanError::AlreadyClosed(current.lifecycle().label()));
    }
    // A backward clock must not produce a transition outside the row's own history
    let at = at.max(current.updated_at());
    let (completed_at, trashed_at) = match lifecycle {
        Lifecycle::Completed => (Some(at), None),
        Lifecycle::Trashed => (None, Some(at)),
        Lifecycle::Open => (None, None),
    };
    Todo::new(
        current.id(),
        current.title().to_owned(),
        current.project_id(),
        current.parent_id(),
        current.tag_ids().to_vec(),
        current.dependency_ids().to_vec(),
        lifecycle,
        current.version().next().map_err(HumanError::Domain)?,
        current.created_at(),
        at,
        completed_at,
        trashed_at,
        current.due().cloned(),
    )
    .map_err(HumanError::Domain)
}

/// The identifier characters a person quotes to name a todo.
#[must_use]
pub fn handle(id: TodoId) -> String {
    let id = id.to_string();
    id.chars().skip(id.chars().count() - HANDLE_LEN).collect()
}

/// One reminder rendered for a terminal.
#[must_use]
pub fn render(todo: &Todo) -> String {
    let state = match todo.lifecycle() {
        Lifecycle::Open => "",
        Lifecycle::Completed => "  (done)",
        Lifecycle::Trashed => "  (trashed)",
    };
    format!(
        "{}  {:<40}  {}{state}",
        handle(todo.id()),
        todo.title(),
        render_due(todo.due())
    )
}

fn render_due(due: Option<&TodoDue>) -> String {
    match due {
        None => "no due date".to_owned(),
        Some(TodoDue::Date { date, .. }) => date.to_string(),
        Some(TodoDue::Timed { at, .. }) => format!(
            "{:04}-{:02}-{:02} {}",
            at.year(),
            at.month(),
            at.day(),
            at.format("%H:%M")
        ),
    }
}
