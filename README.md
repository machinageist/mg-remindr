# mg-todo

`mg-todo` is an independent todo authority under incremental development. The
planned scope includes todos, projects, tags, relationships, lifecycle,
versions, recurrence, reminders, and delivery records; those capabilities are
not all implemented yet.

The current operational persistence slices embed an append-only, checksum-verified
migration ledger with schema-drift checks and independent project, tag, and core
todo repositories. Project, tag, and core todo creation, reads, deterministic lists, and
row-locked optimistic replacement preserve caller-owned IDs, versions, and
timestamps; project persistence also preserves lifecycle. PostgreSQL
configuration remains local-only and the disposable PostgreSQL integration
tests remain explicitly opt-in through `MG_TODO_ALLOW_INTEGRATION_TESTS=1`.

Set `MG_TODO_DATABASE_URL` to a local PostgreSQL URL, then use:

```text
mg-todo migration status|apply
mg-todo project create|find|list|replace
mg-todo tag create|find|list|replace
mg-todo todo create|find|list|replace
```

Create and replace accept complete validated domain objects through `--json`;
replace also requires `--expected-version`. Every successful command emits JSON,
and invalid input, missing records, conflicts, or storage failures exit nonzero
with stable redacted errors.

Parent/tag/dependency relationships, recurrence, reminders, interop export/import, migration/cutover,
compatibility adapters, and agenda integration remain deferred.
