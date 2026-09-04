# Usable reminders implementation plan

Baseline: `5cc516b`

## Dependency order

1. Lifecycle fidelity. Blocks everything: without it the first completion breaks
   the projection permanently.
2. Due values. Makes the calendar integration mean something.
3. Human CLI. Makes daily entry possible.
4. Local database defaults. Removes first-run friction.

## Slice 1: lifecycle fidelity

- `migrations/0010_todo_lifecycle_times.sql`: append `completed_at` and
  `trashed_at` to `todos`, nullable, with checks binding each to its lifecycle and
  to `created_at`. Existing rows keep NULL; no value is invented.
- `src/storage.rs`: extend the version-10 expected column and constraint lists,
  read and write both columns, and keep the checkpoint advance transactional.
- `src/domain.rs`: add both fields to `Todo` and `TodoWire`, and validate that a
  transition time is present exactly when its lifecycle requires it and is never
  earlier than `created_at`.
- `src/interop.rs`: drop `validate_exportable_todo`'s open-only refusal, map
  `Completed` and `Trashed` through the existing `lifecycle` helper, and emit both
  timestamps in the payload.

Verification: `cargo test --all-targets` with `MG_TODO_ALLOW_INTEGRATION_TESTS=1`,
then a live round trip proving a completed todo exports and imports.

## Slice 2: due values

- `migrations/0011_todo_due.sql`: append a due value and its timezone to `todos`.
- `src/domain.rs`: an optional due value serializing as `mg-calr`'s externally
  tagged `Date`/`Timed` enum, validating the IANA timezone and, for timed values,
  that the offset matches the zone at that instant.
- `src/storage.rs` and `src/interop.rs`: persist and export it.

Verification: focused tests, then a live round trip proving a dated todo reaches
`mg-calr agenda` on the right local day.

## Slice 3: human CLI

- `src/main.rs`: top-level `add`, `ls`, `done`, and `rm`, resolving identity,
  version, and timestamps internally and accepting an unambiguous ID prefix. They
  sit beside `todo create|find|list|replace`, which stays the automation surface.
- `--due` accepts `today`, `tomorrow`, a civil date, or a date and local time.
  `--timezone` defaults to the system zone, resolved from `TZ` or `/etc/localtime`,
  and fails closed rather than silently assuming UTC.
- Human output by default, `--json` for automation.
- `--project` is deferred: project selection is not required to keep a reminder,
  and projects have no human creation surface yet.

Verification: CLI contract tests covering due parsing, zone resolution, ambiguous
prefix, and unknown ID.

## Slice 4: local database defaults

- `src/config.rs`: fall back to the local socket authority when no URL is
  configured, keeping the loopback and socket checks intact.

Verification: config tests proving the default resolves and remote URLs still fail.

## Stop conditions

Stop and cut a prerequisite slice rather than expanding if the work needs a
changed ID, rewritten migration SQL, a sibling database connection, calendar
mutation, or a second writable authority for todo state.
