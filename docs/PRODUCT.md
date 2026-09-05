# mg-plan product contract

Status: accepted suite direction; implementation is incremental.

## Product promise

`mg-plan` turns cited understanding into explicit commitments and proves whether those commitments were fulfilled. It is the suite's workflow center, not its technical hub, execution engine, calendar, knowledge store, repository mirror, or artifact warehouse.

The existing `mg-remindr` authority in this repository evolves in place into `mg-plan`. Existing durable IDs and PostgreSQL history remain valid. The `mg-remindr` binary and `mg-remindr` projection contract remain compatibility surfaces until a separately verified cutover removes them.

## Suite authorities

- `mg-brief`: external sources, observations, advisories, and explainable advisory-to-inventory findings.
- `mg-vault`: user-owned knowledge, claims, citations, synthesis, and initially active-learning records.
- `mg-plan`: projects, outcomes, project decisions, work items, dependencies, criteria, gates, verdicts, waivers, work transitions, and initially execution-journal records.
- `mg-calr`: events, recurrence, time blocks, and scheduling responses.
- local observation namespace, initially incubated in `mg-brief`: immutable machine/software observations. Extract `mg-inventory` only after a second consumer or privilege/lifecycle boundary proves the need.

Other capabilities begin as modules or projections: lab inside plan; review inside vault; dev as repository/CI adapters; publish as an approved vault export pipeline; ops/search/dashboard as rebuildable projections; capture as a narrow pending spool.

## Core contract

The suite closes this chain:

`commitment -> attempt -> artifact manifest -> evidence assertion -> verification verdict`

`mg-plan` owns commitments and gate verdicts. Producers own attempts and artifacts. A digest proves artifact integrity, not relevance or gate satisfaction. AI output is a proposal and cannot authorize durable transitions, issue a trusted verdict, waive a gate, execute external mutations, or publish.

## mg-plan aggregate ownership

### Project

A bounded intended outcome. The current `Project` identity is preserved. A project owns its outcome, lifecycle, decisions, milestones, work items, criteria, and gates.

### Work item

The successor vocabulary for `Todo`. Existing `TodoId` values remain durable. Compatibility representations may continue to say `todo`; new plan-native contracts say `work_item`. A work item may be completed operationally, but verified completion is derived from applicable required gates or an explicit waiver.

### Decision

A project-constraining choice with considered options, rationale, status, author, timestamps, supersession history, and typed references. General knowledge conclusions remain in `mg-vault`; repository ADRs remain authoritative in their repositories.

### Criterion and gate

A criterion states what must be true. A gate binds criteria to an exact subject revision and declares permitted evidence kinds, verifier policy, freshness, and required/optional status. A verdict is pass, fail, blocked, inconclusive, invalid-evidence, or waived. Waivers are explicit records, never status edits.

### Execution journal

Initially a module within `mg-plan`: immutable attempts, ordered steps, redacted commands, environment references, observations, and artifact manifests. Extraction to `mg-lab` requires independent reusable protocol/experiment lifecycle pressure.

## Explicit non-ownership

`mg-plan` does not own source contents, vault notes, calendar events, repository state, CI jobs, machine inventory, artifact bytes, service supervision, secrets, public deployment, or federated indexes. It stores typed references and producer-supplied immutable projections.

## Interoperability

Applications never read or write sibling databases. Shared code is restricted to narrow reference/envelope primitives, not shared domain aggregates.

Every exported observation carries producer, schema/version, authority-qualified stable ID, revision, deterministic content identity, provenance, lifecycle, typed links, and completeness diagnostics. Cross-authority references are values, not foreign keys. Missing, stale, conflicted, tombstoned, and unavailable targets are first-class states.

Observation/export envelopes and command/request envelopes are separate. Commands carry an idempotency key and receive an acceptance or rejection receipt plus any resulting object reference. Delivery does not imply acceptance.

Outgoing link assertions are owned by the asserting application; backlinks are rebuildable projections. Exact revisions are required for verification and publication.

## Scheduling boundary

`mg-plan` owns desired scheduling constraints. `mg-calr` owns accepted events and time blocks. A plan scheduling request cannot create or edit calendar rows directly. Event completion never silently completes a work item.

## Migration invariants

- Preserve every existing project/todo/tag identifier and committed migration.
- Append migrations; never rewrite applied migration SQL or history.
- Keep the current `mg-remindr` producer and CLI readable during compatibility phases.
- Add plan-native APIs before changing consumers.
- Export both compatibility and plan-native projections during cutover when necessary; never reuse one producer/revision identity for different bytes.
- Prevalidate migrations, reject overwrite/conflict, verify round trips, and retain rollback evidence.
- Remove compatibility only after all known consumers have migrated and a separate deletion checkpoint passes.

## Deferred and prohibited scope

Deferred: missions, portfolios, arbitrary workflows, reusable lab protocols, standalone review, full ops, multi-destination publishing, broad inventory, and cross-device synchronization.

Do not build: a generic graph engine, central event bus, shared writable suite database, universal evidence lake, repository mirror, automatic remediation, AI-issued verdicts, dashboard-owned workflows, generic plugin marketplace, or commodity email/chat/contacts/file-management features.
