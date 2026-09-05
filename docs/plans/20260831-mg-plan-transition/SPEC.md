# mg-remindr to mg-plan transition specification

Status: active change specification

## Desired outcome

Evolve this repository's existing project/todo PostgreSQL authority into the `mg-plan` authority without invalidating IDs, migration history, the `mg-remindr` CLI, or the immutable todo projection already consumed by `mg-calr`.

## Current implementation truth

At baseline `6733cccac13f6ab6790c54486d4a11739bfbf454`, the repository has validated UUIDv7 project, tag, and todo types; append-only checksum-verified PostgreSQL migrations; project/tag/todo repositories; optimistic versioned replacement; stable redacted CLI failures; and passing unit, CLI, migration, and disposable PostgreSQL tests. Parent/tag/dependency persistence, interop export, plan decisions, criteria, gates, verdicts, attempts, evidence manifests, and scheduling requests are absent.

`mg-calr` already validates and consumes immutable `mg-remindr` projections. It must remain a consumer, not regain todo/work authority.

The producer and consumer models are not yet lossless peers. `mg-remindr` persists a lifecycle enum and `updated_at`, while the current `mg-calr` projection payload expects distinct `completed_at` and `trashed_at` timestamps plus calendar-specific optional fields. `updated_at` must not be relabeled as a lifecycle transition time. The first exporter may produce a complete snapshot only for states it can represent without invention; otherwise it must emit an incomplete/degraded snapshot that `mg-calr` refuses for agenda use. A later append-only lifecycle-fidelity slice must resolve this gap.

The current authority also lacks one monotonic aggregate revision. Per-row versions and maximum timestamps are not a safe substitute: a different row can change without increasing either maximum, and caller-supplied timestamps need not establish a strict total order. Before export, an append-only migration must establish a transactionally advanced authority revision/checkpoint that covers every authoritative project, tag, and todo mutation. Snapshot reads must obtain that checkpoint and all rows in the same repeatable-read transaction.

## First transition slice

Establish the monotonic authority revision/checkpoint and prove that successful mutations advance it exactly as specified while rejected or rolled-back mutations do not. The revision mechanism must also cover writes that bypass one repository method but still cross the PostgreSQL authority boundary; otherwise an exported `producer_revision` could silently remain unchanged.

## Second transition slice

Create a deterministic, read-only `mg-remindr` interoperability snapshot from the existing authority. The contract must be consumable by `mg-calr` without direct PostgreSQL access and must establish the migration seam for a later plan-native producer.

The slice includes:

- one repeatable-read PostgreSQL snapshot boundary;
- authority-qualified IDs and immutable revisions for projects, tags, todos, and relationships that are actually persisted;
- producer/schema/version and deterministic source revision/export identity;
- explicit completeness and diagnostics;
- stable ordering;
- no invented relationship timestamps;
- CLI export requiring explicit JSON;
- no import, sync, scheduling mutation, plan-native entity expansion, or direct calendar write.

## Acceptance criteria

1. Unchanged authoritative rows yield identical canonical snapshot identity.
2. Every exported row can be traced to its current PostgreSQL authority and validated domain constructor.
3. Snapshot acquisition reads the monotonic authority checkpoint and all exported rows in one repeatable-read transaction.
4. Export refuses schema or stored-data drift and marks semantically unrepresentable authority state incomplete instead of publishing false completeness.
5. Empty state exports truthfully and deterministically.
6. Unknown fields/version skew fail according to a tested contract.
7. The existing `mg-remindr` CLI and storage behavior remain compatible.
8. `mg-calr` can validate a faithfully representable producer fixture without adapting private database state; a fixture with lifecycle timestamp loss is rejected as incomplete.
9. Focused tests, all targets, strict Clippy, formatting, diff hygiene, and an independent blind review pass.

## Non-goals

- Renaming PostgreSQL schemas or tables
- Replacing `TodoId`
- Adding decisions, gates, attempts, or evidence in this slice
- Adding a shared cross-suite domain crate
- Writing another application's files or database
- Removing the calendar repository's legacy todo tables before a dedicated migration/cutover slice
