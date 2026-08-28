# mg-todo

`mg-todo` is an independent todo authority under incremental development. The
planned scope includes todos, projects, tags, relationships, lifecycle,
versions, recurrence, reminders, and delivery records; those capabilities are
not all implemented yet.

The current operational persistence slice embeds an append-only migration ledger
and the first project repository. Project creation, reads, deterministic lists,
and row-locked optimistic replacement preserve caller-owned IDs, versions,
lifecycle, and timestamps. PostgreSQL configuration remains local-only and the
disposable PostgreSQL integration test remains explicitly opt-in through
`MG_TODO_ALLOW_INTEGRATION_TESTS=1`.

Todo/tag/relationship persistence, interop export/import, migration/cutover,
compatibility adapters, and agenda integration remain deferred.
