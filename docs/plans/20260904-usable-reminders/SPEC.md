# Usable reminders specification

Status: slices 1-4 delivered and verified end to end against the local authority

## Desired outcome

A person can keep reminders in `mg-remindr` and see the dated ones on the `mg-calr`
agenda, using the shipped CLIs and nothing else. Today none of that is possible,
for reasons proven against a live local PostgreSQL authority on 2026-09-04.

## Current implementation truth

Baseline `5cc516b`, which already repaired migration application. Verified by
walking the documented end-to-end path against `mg_todo` and `mg_calr`:

- `mg-calr` is operable. Migrations apply, calendars and events are created from
  flags, the day agenda renders, and `interop import-todo` validates and stores a
  producer snapshot crash-safely.
- `mg-remindr` migrations now apply and `interop export` produces a snapshot that
  `mg-calr` accepts.
- `Todo` has no due value. `todo_payload` hardcodes `due`, `recurrence`,
  `reminders`, `priority`, and `notes` to empty. `mg-calr`'s `agenda_due_instances`
  returns nothing for a todo without a due value, so no `mg-remindr` record can ever
  appear on a day agenda.
- `validate_exportable_todo` refuses any todo that is not `Open`. Completing or
  trashing one todo makes every later export fail, so the calendar projection can
  never be refreshed again.
- The `todo_recurrence`, `todo_reminders`, and `todo_reminder_deliveries` tables
  exist and `src/recurrence.rs` and `src/reminder.rs` hold validated types, but
  neither is part of the `Todo` aggregate, its persistence, or its export.
- Creating a todo requires hand-writing a complete domain object including a UUID,
  a version, and both timestamps. Completing one requires `todo replace` with the
  full object and `--expected-version`.
- There is no default database URL, and `postgresql:///mg_todo` is rejected because
  the local-only check requires an explicit host.

## Slices

### Slice 1: lifecycle fidelity

Persist `completed_at` and `trashed_at` on the todo authority and export them, so
a completed or trashed todo is representable and export stops failing closed.
Transition times are recorded when a transition is stored; they are never
manufactured from `updated_at` for rows that predate the column.

### Slice 2: due values

Give `Todo` an optional due value in the representation `mg-calr` already parses,
persist it, and export it. A dated todo must reach the agenda.

### Slice 3: human CLI

Add flag-driven `add`, `list`, `done`, and `rm` surfaces that own identity,
version, and timestamp generation. `create`/`replace` remain for automation.

### Slice 4: local defaults

Resolve a default local database URL so a first run needs no environment variable,
without weakening the local-only boundary.

## Acceptance criteria

1. Completing a todo leaves `interop export` succeeding, and the exported record
   carries the transition time the authority stored.
2. A todo given a due date appears on the `mg-calr` agenda for that day, in the
   requested timezone, after an explicit export and import.
3. Completing that todo removes it from the default agenda and `--include-completed`
   restores it.
4. A person can add, list, and complete a reminder without writing JSON, a UUID,
   a version, or a timestamp.
5. A fresh shell with no `MG_REMINDR_DATABASE_URL` reaches the local authority; a
   non-loopback URL is still rejected.
6. Focused tests, all targets, strict Clippy, formatting, and diff hygiene pass,
   with the disposable-PostgreSQL suite enabled.

## Delivered

All four slices are implemented, and the acceptance criteria were walked against
the live `mg_todo` and `mg_calr` databases through the suite's own
`geist-sync-todo-projection` bridge: a reminder added with `mg-remindr add` appears on
`mg-calr agenda` for its day in the requested zone, and completing it removes it.

`mg-calr`'s agenda rendering was repaired alongside, in that repository.

## Remaining

- Recurrence and reminder delivery have persistence but are absent from the todo
  aggregate and its export.
- Projection refresh is manual, by design.

## Non-goals

- Recurrence and reminder delivery in the export payload; those stay empty and
  explicitly unowned until their own slice.
- Changing `TodoId`, rewriting applied migration SQL, or renaming tables.
- Any cross-database read or write.
- Plan-native vocabulary, decisions, gates, verdicts, or evidence.
- Notification delivery, a TUI, or synchronization.
