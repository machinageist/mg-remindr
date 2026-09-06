# mg-remindr

`mg-remindr` is the local PostgreSQL todo authority for the Geist suite. It owns
todos, projects, tags, relationships, lifecycle, versions, and the recorded
transition times behind them, and produces the immutable projection `mg-calr`
reads for its agenda. Recurrence and reminder delivery have persistence but are
not yet part of the todo aggregate or its export.

## Keeping reminders

```text
mg-remindr add "Pay rent" --due today
mg-remindr add "Dentist" --due 2026-09-08T14:00
mg-remindr ls
mg-remindr done <handle>
mg-remindr rm <handle>
mg-remindr restore <handle>
```

`--due` accepts `today`, `tomorrow`, `YYYY-MM-DD`, or `YYYY-MM-DDTHH:MM`.
`--timezone` names the IANA zone the due value is written in and defaults to the
system zone resolved from `TZ` or `/etc/localtime`; a local time that does not
exist or repeats across a transition is refused rather than guessed. `ls` shows
open reminders; `--all` includes completed and trashed ones, which is how a
handle is found for `restore`. Add `--json` to any
of these for the stored domain object.

The handle in `ls` is the identifier's trailing characters. UUIDv7 leads with a
millisecond timestamp, so reminders added in the same second share their leading
digits; `done` and `rm` accept the handle, a full identifier, or any unambiguous
trailing or leading part of one.

## Automation surface

```text
mg-remindr migration status|apply
mg-remindr project create|find|list|replace
mg-remindr tag create|find|list|replace
mg-remindr todo create|find|list|replace
mg-remindr interop export
```

Create and replace accept complete validated domain objects through `--json`;
replace also requires `--expected-version`. Every command emits JSON, and invalid
input, missing records, conflicts, or storage failures exit nonzero with stable
redacted errors.

`interop export` writes one deterministic `mg.interop/1` snapshot read inside a
single repeatable-read transaction, carrying the monotonic authority revision.

## Reaching the calendar

`mg-calr` never opens this database. Refresh its agenda projection explicitly:

```text
mg-remindr interop export > snapshot.json
mg-calr interop import-todo --input snapshot.json \
  --store ~/.local/share/mg-calr/todo-projection.json
mg-calr agenda --start 2026-09-04 --end 2026-09-05 --timezone America/New_York
```

Only reminders with a due value appear on a day agenda. Completed reminders are
hidden unless `mg-calr agenda --include-completed` is passed.

## Database

Configuration is local-only: a localhost, loopback, or Unix-socket host. With
nothing configured the authority is `postgresql:///mg_todo?host=/run/postgresql`,
which expects a peer-authenticated role and database an administrator has already
provisioned:

```bash
sudo -u postgres createuser --login "$USER"
sudo -u postgres createdb --owner "$USER" mg_todo
mg-remindr migration apply  # run unprivileged, not through sudo
```

`--database-url`, then `MG_REMINDR_DATABASE_URL`, then
`$XDG_CONFIG_HOME/mg-remindr/config.toml` override that default. A remote host is
rejected before any connection is attempted.

## Development

```bash
cargo fmt --all -- --check
TMPDIR=/dev/shm cargo clippy --all-targets --all-features -- -D warnings
MG_REMINDR_ALLOW_INTEGRATION_TESTS=1 TMPDIR=/dev/shm cargo test --all-targets
git diff --check
```

The disposable-PostgreSQL suite is opt-in through `MG_REMINDR_ALLOW_INTEGRATION_TESTS=1`
and starts its own server; leaving it off hides migration and schema regressions.

## Deferred

Recurrence and reminders in the todo aggregate and its export, reminder delivery,
plan-native vocabulary, interop import, migration cutover, and compatibility
adapter removal.

## License

MIT. See `LICENSE`.
