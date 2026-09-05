# mg-plan transition implementation plan

Baseline: `6733cccac13f6ab6790c54486d4a11739bfbf454`

## Locked decisions

- Evolve this repository in place into `mg-plan`.
- Preserve `mg-todo` as a compatibility CLI and projection producer during migration.
- `mg-plan` owns projects/work; `mg-calr` owns time.
- No cross-database reads or writes.
- No automatic remediation or AI-authorized transition.

## Dependency order

1. Document product and migration authority. Status: in progress.
2. Add a monotonic PostgreSQL authority revision/checkpoint covering every project, tag, and todo mutation.
3. Export the current mg-todo authority as one deterministic immutable snapshot.
4. Prove mg-calr validates faithfully representable producer fixtures and rejects lifecycle-loss fixtures without direct database access.
5. Add append-only lifecycle fidelity and update the consumer contract without inventing historical transition timestamps.
6. Add a plan-native compatibility facade while preserving durable IDs.
7. Add typed project outcomes and project decisions.
8. Add work-item dependencies with transactional cycle validation.
9. Add criteria, gates, verdicts, and explicit waivers.
10. Add scheduling request/receipt records; mg-calr remains the event authority.
11. Add execution attempts and artifact manifests.
12. Close the Brief -> Vault -> Plan -> Calr -> Attempt -> Verdict -> Vault retrospective loop.
13. Migrate known consumers to the plan-native producer.
14. Remove compatibility only in a separately approved and verified deletion slice.

## Immediate corrective slice: monotonic authority revision

Add one append-only migration and behavior tests establishing a durable positive revision/checkpoint for the complete mg-todo authority. It must advance atomically with successful project, tag, and todo changes, remain unchanged after validation/database rollback, survive restart, reject drift, and be readable in the same transaction later used by snapshot export. Do not derive it from record counts, maximum per-row versions, wall-clock export time, or a truncated content digest.

## Following slice: deterministic compatibility export

Likely files:

- `src/domain.rs`: only if narrow exported reference types are needed; do not duplicate canonical IDs.
- `src/storage.rs`: one repeatable-read read model over existing authority.
- `src/interop.rs`: canonical snapshot construction and validation.
- `src/main.rs`: explicit `interop export --json` surface.
- `tests/interop_contract.rs`: deterministic, malformed/stale/schema, and CLI boundary tests.
- PostgreSQL integration test: prove one-transaction acquisition and round trip.

The compatibility payload may supply only schema-defined neutral defaults for fields that `mg-todo` does not own (for example no due value or reminders). It must not manufacture completion/trash timestamps from `updated_at`. Any authoritative row that cannot be represented faithfully makes the snapshot explicitly incomplete.

Verification:

- `TMPDIR=/home/mgeist/.tmp-hermes cargo test --test interop_contract`
- `TMPDIR=/home/mgeist/.tmp-hermes cargo test --all-targets`
- `TMPDIR=/home/mgeist/.tmp-hermes cargo clippy --all-targets -- -D warnings`
- `cargo fmt --check`
- `git diff --check`
- fresh independent blind review against the exact baseline and diff

## Stop conditions

Stop and create a prerequisite slice rather than expanding scope if implementation requires:

- changing existing IDs or applied migration SQL;
- a cross-application database connection;
- a generic shared entity/graph schema;
- calendar mutation;
- a second writable authority for project or todo state;
- copying artifact bytes into plan storage;
- silently dropping malformed or unsupported authoritative rows.
