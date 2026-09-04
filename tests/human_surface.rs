use chrono::{DateTime, NaiveDate, Utc};
use mg_todo::{
    domain::{Lifecycle, Todo, TodoDue, TodoId, Version},
    human::{HumanError, close, handle, new_todo, parse_due, render, resolve_handle, resolve_zone},
};

fn instant(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 4).unwrap()
}

fn todo(title: &str, due: Option<TodoDue>) -> Todo {
    new_todo(title.to_owned(), due, instant("2026-09-04T12:00:00Z")).unwrap()
}

#[test]
fn due_values_are_written_the_way_a_person_writes_them() {
    let zone = "America/New_York";
    assert_eq!(
        parse_due("today", zone, today()).unwrap(),
        TodoDue::date(today(), zone.to_owned()).unwrap()
    );
    assert_eq!(
        parse_due("Tomorrow", zone, today()).unwrap(),
        TodoDue::date(
            NaiveDate::from_ymd_opt(2026, 9, 5).unwrap(),
            zone.to_owned()
        )
        .unwrap()
    );
    assert_eq!(
        parse_due(" 2026-12-25 ", zone, today()).unwrap(),
        TodoDue::date(
            NaiveDate::from_ymd_opt(2026, 12, 25).unwrap(),
            zone.to_owned()
        )
        .unwrap()
    );

    // A local wall time is anchored to its zone's offset on that date, not to UTC
    let summer = parse_due("2026-09-08T14:00", zone, today()).unwrap();
    let winter = parse_due("2026-01-08 14:00", zone, today()).unwrap();
    match (summer, winter) {
        (TodoDue::Timed { at: summer, .. }, TodoDue::Timed { at: winter, .. }) => {
            assert_eq!(summer.to_rfc3339(), "2026-09-08T14:00:00-04:00");
            assert_eq!(winter.to_rfc3339(), "2026-01-08T14:00:00-05:00");
        }
        other => panic!("expected timed due values, got {other:?}"),
    }
}

#[test]
fn unreadable_and_unnamed_due_values_fail_closed() {
    assert_eq!(
        parse_due("next thursday", "America/New_York", today()),
        Err(HumanError::UnreadableDue)
    );
    assert_eq!(
        parse_due("today", "Mars/Olympus", today()),
        Err(HumanError::UnknownZone)
    );
    // 02:30 does not exist in New York on the spring transition
    assert_eq!(
        parse_due("2026-03-08T02:30", "America/New_York", today()),
        Err(HumanError::UnreadableDue)
    );
}

#[test]
fn zone_resolution_prefers_the_request_then_the_environment() {
    assert_eq!(
        resolve_zone(Some("Europe/Berlin")).unwrap(),
        "Europe/Berlin"
    );
    assert_eq!(
        resolve_zone(Some("Mars/Olympus")),
        Err(HumanError::UnknownZone)
    );
    // The system fallback must produce a zone this machine can actually name
    let resolved = resolve_zone(None).unwrap();
    assert!(resolved.parse::<chrono_tz::Tz>().is_ok());
}

#[test]
fn handles_are_stable_across_identifiers_that_share_a_timestamp() {
    let first = todo("first", None);
    let second = todo("second", None);
    assert_ne!(handle(first.id()), handle(second.id()));
    assert_eq!(handle(first.id()).len(), 8);
    assert!(first.id().to_string().ends_with(&handle(first.id())));
}

#[test]
fn a_handle_names_one_todo_or_refuses_to_guess() {
    let first = todo("first", None);
    let second = todo("second", None);
    let todos = vec![first.clone(), second.clone()];

    assert_eq!(
        resolve_handle(&todos, &handle(first.id())).unwrap(),
        first.id()
    );
    assert_eq!(
        resolve_handle(&todos, &first.id().to_string()).unwrap(),
        first.id()
    );
    assert_eq!(
        resolve_handle(&todos, "  "),
        Err(HumanError::NoMatch("  ".to_owned()))
    );
    assert_eq!(
        resolve_handle(&todos, "zzzzzzzz"),
        Err(HumanError::NoMatch("zzzzzzzz".to_owned()))
    );

    // Both identifiers begin with the UUIDv7 millisecond timestamp
    let shared = &first.id().to_string()[..8];
    assert!(second.id().to_string().starts_with(shared));
    assert!(matches!(
        resolve_handle(&todos, shared),
        Err(HumanError::Ambiguous(_, matches)) if matches.len() == 2
    ));
}

#[test]
fn closing_advances_the_version_and_records_the_transition_time() {
    let open = todo("pay rent", None);
    let at = instant("2026-09-04T13:00:00Z");

    let done = close(&open, Lifecycle::Completed, at).unwrap();
    assert_eq!(done.lifecycle(), Lifecycle::Completed);
    assert_eq!(done.completed_at(), Some(at));
    assert_eq!(done.trashed_at(), None);
    assert_eq!(done.version(), Version::try_from_value(2).unwrap());
    assert_eq!(done.created_at(), open.created_at());

    let trashed = close(&open, Lifecycle::Trashed, at).unwrap();
    assert_eq!(trashed.trashed_at(), Some(at));
    assert_eq!(trashed.completed_at(), None);

    assert_eq!(
        close(&done, Lifecycle::Completed, at),
        Err(HumanError::AlreadyClosed("completed"))
    );
}

#[test]
fn a_backward_clock_cannot_place_a_transition_outside_the_rows_history() {
    let open = todo("pay rent", None);
    let earlier = instant("2026-09-04T11:00:00Z");
    let done = close(&open, Lifecycle::Completed, earlier).unwrap();
    assert_eq!(done.completed_at(), Some(open.updated_at()));
    assert_eq!(done.updated_at(), open.updated_at());
}

#[test]
fn added_todos_are_open_with_generated_identity_and_matching_timestamps() {
    let at = instant("2026-09-04T12:00:00Z");
    let due = TodoDue::date(today(), "America/New_York".to_owned()).unwrap();
    let todo = new_todo("water the plants".to_owned(), Some(due.clone()), at).unwrap();
    assert_eq!(todo.lifecycle(), Lifecycle::Open);
    assert_eq!(todo.version(), Version::new());
    assert_eq!(todo.created_at(), at);
    assert_eq!(todo.updated_at(), at);
    assert_eq!(todo.due(), Some(&due));
    assert_ne!(todo.id(), TodoId::new());

    assert!(matches!(
        new_todo("   ".to_owned(), None, at),
        Err(HumanError::Domain(_))
    ));
}

#[test]
fn rendering_states_the_handle_the_due_value_and_a_closed_lifecycle() {
    let dated = todo(
        "pay rent",
        Some(TodoDue::date(today(), "America/New_York".to_owned()).unwrap()),
    );
    let line = render(&dated);
    assert!(line.starts_with(&handle(dated.id())));
    assert!(line.contains("pay rent"));
    assert!(line.contains("2026-09-04"));
    assert!(!line.contains("(done)"));

    assert!(render(&todo("read", None)).contains("no due date"));
    let done = close(
        &dated,
        Lifecycle::Completed,
        instant("2026-09-04T13:00:00Z"),
    )
    .unwrap();
    assert!(render(&done).contains("(done)"));
}
