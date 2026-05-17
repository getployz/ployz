---
title: Slice 005 Fact Projection And Snapshot Plan
status: active
created: 2026-05-17
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-003-authority-islands.md
  - MVP/slice-004-authority-bridge.md
---

# Slice 005 Fact Projection And Snapshot Plan

## Problem Frame

The MVP bus can now express island-local authority and explicit cross-island
bridges. The next missing proof is that durable facts are useful product truth:
business facts should reduce deterministically into local SQLite projections
and gateway/DNS snapshot files without turning SQLite into the source of truth.

The single proof target for this slice is:

> Route, DNS, service, and node facts reduce into deterministic SQLite
> projections and atomic gateway/DNS snapshot files; deleting the projection
> database and dropping notification delivery still converges by full rebuild
> from facts.

This is not a reduction of the MVP. It is `E2E-4a`: the projection-contract
half of E2E-4, with large-load logical-node proof. E2E-4 remains open until
`E2E-4b` proves real docs-backed replication, propagation metrics, and remote
service-registry projections. The final MVP still needs actual iroh-docs
anti-entropy, iroh transport, machine join, deploy, WireGuard, and gateway/DNS
process continuity. This slice creates the contract those later pieces must
satisfy.

## Why This Is Next

The bridge slice proved that islands can share selected services and streams
without merging truth. The next highest-leverage boundary is the truth-to-view
pipeline:

- facts are durable, mostly immutable operator truth,
- projections are disposable local query state,
- snapshots are the data-plane input that gateway/DNS roles can keep serving.

Without this boundary, machine join and deploy work would either store mutable
heads directly in SQLite or make gateway/DNS depend on live control-plane
availability again. Proving projection now also gives a small, representative
business flow for the semantic-leverage gate: route commit to gateway/DNS
snapshot should read as business code, not storage/transport ceremony.

## Requirements Traceability

- `VISION.md`: the daemon is disposable; the data plane keeps serving; state
  transitions must have visible outcomes instead of hidden reconcilers.
- `MVP/overall-plan.md`: iroh-docs is durable fact truth, SQLite is a
  rebuildable projection, and every slice must add E2E/stress proof under
  `MVP/`.
- `MVP/architecture.md`: fact keys are mostly immutable; reducers compute
  current state; correctness must not depend on `applied_fact`; snapshots are
  the future input model for gateway/DNS.
- `MVP/e2e-proof-plan.md`: E2E-4 requires node/service/route/DNS facts,
  deterministic rebuild after deleting `projections.sqlite`, dropped
  notification recovery, conflict/authorization visibility, and projection
  metrics.
- `MVP/primitive-decisions.md`: the current in-memory fact harness should
  become a backend behind the same fact contract, not a parallel source of
  truth.
- `MVP/slice-004-authority-bridge.md`: laptop/prod isolation must remain true;
  projection cannot create a side channel that lets one island read or mutate
  another island's facts.

## Scope

Implement MVP-local fact projection semantics:

- typed fact payloads for node joins, service registrations, route commits,
  gateway commits, and DNS commits,
- a fact-store read/list contract that can later be backed by iroh-docs,
- deterministic full reduction from facts into projection state,
- SQLite projection persistence that is always rebuildable from facts,
- atomic JSON snapshot writers for `gateway.snapshot` and `dns.snapshot`,
- a Kameo-owned projection actor that runs rebuilds and exposes structured
  projection status,
- E2E proof for rebuild after deleting SQLite,
- E2E proof that dropped fact notifications are only hints and a full pass
  catches up,
- 200, 1,000, and 10,000 logical-node stress runs with node principals, grants,
  node/service facts, projection actors, and projection/snapshot metrics.

Out of scope for this slice:

- adding an `iroh-docs` dependency or real network anti-entropy,
- changing root workspace Rust version or repo CI toolchain,
- modifying existing gateway/DNS binaries outside `MVP/`,
- parsing the existing gateway/DNS runtime snapshot structs directly,
- durable deploy phase commits or `store.pin_fact`,
- machine join/remove and WireGuard reconciliation,
- a mutable SQL event log, gap repair, or global total order.

Non-closure gate:

- this slice must not mark E2E-4 complete,
- no report or test name should claim real fact replication is proven,
- `MVP/e2e-proof-plan.md` remains open for docs-backed anti-entropy,
  propagation latency, and remote projection proof.

## Current Patterns To Preserve

- Business-facing mutation continues through `BusActorHandle` and grants.
- Facts remain island-scoped and authorized before mutation.
- Authorization failures and projection failures remain structured error
  variants. Tests must branch on variants, not display strings.
- The synchronous in-memory bus remains a harness detail; future business code
  should call actor handles or projection handles.
- All new code and docs stay under `MVP/`.
- Existing gateway/DNS crates are reference material only. The MVP snapshot
  schema should be readable and self-contained until migration is explicit.

## Crate Scout

This slice would otherwise need SQLite wrappers, atomic file replacement,
content hashes, path handling, error boilerplate, serialization, and perhaps
real docs replication.

Checked options:

- `iroh-docs`: latest `0.99.0` is the right long-term fact replication
  substrate. It models replicas as signed entries whose values are BLAKE3
  content hashes, syncs by range-based set reconciliation, has persistent redb
  storage, and composes with iroh/blobs/gossip. However `cargo info
  iroh-docs@0.99.0` reports Rust 1.91, while this repo and `MVP/` currently
  declare Rust 1.88 and PR CI installs Rust 1.88. Version `0.95.0` has Rust
  1.85 compatibility, but the current iroh line is in a 1.0 RC transition with
  API churn. Decision: do not bind reducers to `iroh-docs` in this slice. Add
  a narrow fact-store contract and keep the in-memory replicated harness behind
  it, then plan an explicit iroh-docs adapter/toolchain slice.
- `iroh` / `irpc-iroh`: defer. This slice is about truth-to-view semantics,
  not transport. The `iroh` 1.0 RC is promising, but it shares the Rust 1.91
  compatibility decision.
- `rusqlite`: adopt for projection storage. It is a small ergonomic SQLite
  wrapper, enough for local rebuildable query tables without inventing a SQL
  layer.
- `tempfile`: adopt for same-directory temporary files followed by `persist`
  when writing snapshots. This keeps atomic replacement boring and avoids
  partially-written snapshots replacing the last good file.
- `camino`: defer unless implementation becomes path-heavy. The current slice
  only needs a few internal paths, so `std::path` should stay simple.
- `blake3`: adopt. Fact payload bytes become real in this slice, iroh-docs and
  iroh-blobs both use BLAKE3-shaped content hashes, and this avoids continuing
  with arbitrary test strings as content IDs.
- `thiserror`: adopt for `mvp-projection` if hand-written error enums become
  noisy. The crate does not leak into public API shape and keeps structured
  errors readable.
- `rmp-serde`: defer. MessagePack may be useful later for compact network
  payloads, but readable JSON snapshots are better while gateway/DNS snapshot
  semantics are still being proved.

Sources:

- iroh-docs crate docs describe signed replica entries, BLAKE3 content hashes,
  range-based set reconciliation, redb-backed storage, and dependency on
  iroh/blobs/gossip:
  <https://docs.rs/iroh-docs/latest/iroh_docs/>
- iroh crate docs describe peer-to-peer QUIC connectivity, hole punching,
  relay fallback, and streams:
  <https://docs.rs/iroh/latest/iroh/>
- iroh 1.0 RC notes describe ongoing API refinement and public API movement:
  <https://www.iroh.computer/blog/iroh-1-0-0-rc-0>
- rusqlite is an ergonomic SQLite wrapper:
  <https://docs.rs/rusqlite/latest/rusqlite/>
- tempfile documents `NamedTempFile::new_in` and `persist` for named temp-file
  workflows:
  <https://docs.rs/tempfile/latest/tempfile/struct.NamedTempFile.html>
- blake3 is the official Rust implementation of BLAKE3:
  <https://docs.rs/blake3/latest/blake3/>
- thiserror derives structured `std::error::Error` implementations without
  changing public API shape:
  <https://docs.rs/thiserror/latest/thiserror/>

## Key Technical Decisions

### Projection Depends On A Fact Contract, Not A Store Implementation

Reducers should consume a minimal fact source owned by `mvp_projection`:

```text
trait FactSource {
    list_candidates(island, pattern, session) -> ordered FactCandidate entries
    read_payload(island, key, content_hash, session) -> bytes
}
```

`mvp_projection` gets an adapter over the bus fact API now. An iroh-docs
adapter can satisfy the same trait later by listing replica entries and fetching
blob content. Reducers must not know whether the fact came from an in-memory
harness, iroh-docs, redb, or another peer.

The seam is intentionally wider than key/hash bytes. It must carry:

- fact island,
- fact key,
- author principal,
- content hash,
- fact kind,
- created-at logical timestamp or deterministic epoch,
- verification status, including signature/author verification for real
  backends and an explicit MVP-local verified marker for the in-memory harness,
- authorization status,
- conflict status for same-key/different-hash candidates.

This gives the future iroh-docs adapter somewhere to surface unsigned,
unauthorized, malformed, and conflicting replicated entries without changing the
reducer contract.

Rejected candidate metadata is sensitive. Ordinary projection outputs may report
aggregate counts and redacted reason classes for unauthorized, unverified, or
cross-island candidates. Raw rejected fact keys, authors, and hashes must not be
persisted in SQLite, written into snapshots, or returned in ordinary
`ProjectionReport` output. A future admin/operator diagnostic surface can expose
raw details only behind an explicit authorization boundary.

### Fact Payloads Become Real Bytes

The current fact harness only stores a typed content hash. Projection needs the
fact body. Extend the fact contract with payload bytes while preserving
content-addressed semantics:

- payload bytes are serialized deterministically,
- `FactContentHash` is derived from bytes with BLAKE3,
- writing the same key and same hash is idempotent,
- writing the same key and different hash is a conflict,
- payload reads require fact-read permission through the owning island,
- listed facts are either verified envelopes or explicit rejected candidates.

This keeps the future iroh-blobs transition natural without making this slice
pull in the whole blob protocol.

The in-memory harness does not need production cryptographic signatures yet, but
the projection-facing adapter must model verification explicitly. A listed
candidate should be `Verified` only when it passed the MVP-local author/grant
checks. A future iroh-docs adapter should mark candidates verified only after
namespace/author signatures validate. Reducers must ignore `Unverified` or
`Unauthorized` candidates and report them as redacted projection status.

### SQLite Is A Projection Cache Only

SQLite should make projection state queryable and testable. It must never be
required for correctness.

The implementation should support:

- creating an empty projection database,
- rebuilding all projection tables from facts,
- deleting `projections.sqlite` and rebuilding the same state,
- keeping optional `applied_facts` bookkeeping for speed only,
- treating corrupt or missing SQLite as rebuild work, not lost truth.

No mutable "head" table should become authority. Current heads are reduced
views such as "latest route commit by deterministic reducer rules."

### Reducers Are Pure And Deterministic

Projection logic should be testable without SQLite, actors, or files. Given an
ordered fact set and payload lookup, the reducer returns a typed
`ProjectionState` plus structured ignored-fact statuses.

Rules:

- sort by island, fact key, and content hash before reducing,
- never use wall-clock order to choose truth,
- choose current route/DNS/gateway state from explicit commit facts,
- ignore malformed, unverified, unauthorized, conflicting, or tombstoned facts with
  operator-visible status,
- produce stable ordering in snapshot output.

### Snapshots Are Atomically Replaced

Snapshot writers should serialize complete JSON into a temporary file in the
same directory, flush it, and persist/rename it over the target. A failed write
must leave the previous snapshot file untouched.

Snapshot paths are a trust boundary. This slice should use explicit
MVP-controlled directories, create parent directories with restrictive
permissions where the platform supports it, reject symlinked snapshot targets,
and set restrictive file modes where the platform supports it. The next
gateway/DNS loader slice will validate schema version, island, and source
commit hashes before replacing an in-memory last-good snapshot.

This slice does not need the existing gateway/DNS binaries to load the files
yet. It must prove the files are complete, stable, parseable by an MVP-local
loader, and safe enough for the next gateway/DNS process-role slice. This slice
tests loader rejection of corrupt files, not gateway/DNS last-good in-memory
replacement semantics. The next slice must either wire gateway/DNS snapshot
loading under `MVP/` or explicitly justify why docs-backed replication is the
more blocking proof.

### Notifications Are Hints

The projection actor may accept "fact inserted" wakeups, but correctness comes
from a full fact listing pass. Tests should intentionally drop wakeups, run a
full pass, and assert that SQLite and snapshots converge.

### Actor Ownership Starts Here

The projection pipeline should have a Kameo-owned boundary:

- `ProjectionActor` owns the projection store path, snapshot paths, and status,
- `ProjectionActor` is constructed for exactly one `IslandId` and one
  fact-read-authorized projection principal/session,
- callers send `ProjectOnce` or `FactHint` messages,
- `ProjectOnce` does not accept an arbitrary island parameter,
- projection work returns structured success/failure to the foreground caller,
- blocking SQLite/file work is either short and bounded or delegated outside
  the mailbox so the actor does not become a hidden global lock.

## Implementation Units

### U1: Extend The Fact Contract With Payloads And Listing

Files:

- Modify `MVP/bus/src/facts.rs`
- Modify `MVP/bus/src/memory.rs`
- Modify `MVP/bus/src/actor.rs`
- Modify `MVP/bus/src/error.rs`
- Modify `MVP/bus/src/lib.rs`
- Modify tests in `MVP/bus/src/facts.rs`, `MVP/bus/src/memory.rs`, and
  `MVP/bus/src/actor.rs`

Goal:

Facts carry retrievable payload bytes and can be listed deterministically by
island/prefix while preserving existing authorization and conflict semantics.

Approach:

- Add a small `FactPayload`/`FactBody` type around shared bytes.
- Derive `FactContentHash` from BLAKE3 bytes for new writes.
- Keep compatibility helpers for existing tests that write only a hash, or
  update tests to write payloads where behavior depends on content.
- Add fact listing on the actor handle, gated by fact-read permission.
- Return structured payload-missing or conflict errors rather than strings.

Test scenarios:

- Writing a fact with payload stores a BLAKE3 hash and returns the same payload
  on read.
- Rewriting the same key with identical payload is idempotent.
- Rewriting the same key with different payload returns `FactConflict`.
- A reader without fact-read permission cannot read or list payload-bearing
  facts.
- Listing by prefix is deterministic and island-scoped.
- Existing direct laptop-to-prod fact write denial remains true.

Verification:

- `cd MVP && cargo test -p mvp-bus`
- `cd MVP && cargo run -p mvp-e2e -- authority-contract`
- `cd MVP && cargo run -p mvp-e2e -- bridge-contract`

### U2: Add The Projection Crate And Pure Reducer

Files:

- Modify `MVP/Cargo.toml`
- Add `MVP/projection/Cargo.toml`
- Add `MVP/projection/src/lib.rs`
- Add `MVP/projection/src/facts.rs`
- Add `MVP/projection/src/model.rs`
- Add `MVP/projection/src/reducer.rs`
- Add `MVP/projection/src/source.rs`
- Add `MVP/projection/src/bus_source.rs`
- Add tests in `MVP/projection/src/reducer.rs`

Goal:

Define the typed projection fact payloads and a pure reducer that produces
`ProjectionState` without SQLite or file I/O.

Approach:

- Introduce typed payloads for node-join fact projection, service
  registration, route commit, gateway commit, and DNS commit.
- Keep IDs typed in the MVP crate instead of using raw strings inside reducer
  logic.
- Use `BTreeMap`/sorted vectors for deterministic outputs.
- Add the projection-facing `FactSource` trait and a bus-backed adapter rather
  than making reducers depend directly on `BusActorHandle`.
- Carry explicit `Verified`, `Unverified`, and `Unauthorized` candidate status
  so future replicated backends can expose invalid entries without changing the
  reducer API.
- Return `ProjectionStatus` entries for ignored facts, malformed payloads,
  unverified candidates, unauthorized candidates, conflicts, and unsupported
  fact kinds.

Terminology:

- "Node join fact projection" means projecting an already-written node fact into
  a local view. The actual machine join workflow, invite flow, iroh dialing, and
  WireGuard reconciliation remain out of scope for this slice.

Test scenarios:

- Node join facts reduce into active node projection rows.
- Service registration facts reduce into service registry projection rows.
- Route commit facts produce gateway routes and old-backend drain metadata.
- Gateway commit facts select the route commit that becomes the gateway
  snapshot source.
- DNS commit facts produce DNS records.
- Cross-island candidates are ignored if they appear in the configured island's
  fact source.
- Shuffled fact input produces byte-identical `ProjectionState`.
- Malformed payloads do not panic and are reported in status.
- Unverified and unauthorized listed candidates do not reach SQLite or
  snapshots and are reported in status.
- Ordinary reducer status redacts raw rejected fact keys, authors, and hashes
  for unauthorized, unverified, and cross-island candidates.

Verification:

- `cd MVP && cargo test -p mvp-projection`

### U3: Persist Rebuildable SQLite Projections

Files:

- Add `MVP/projection/src/sqlite.rs`
- Add tests in `MVP/projection/src/sqlite.rs`

Goal:

Persist `ProjectionState` into `projections.sqlite` as rebuildable query
tables, with no correctness dependency on prior DB contents.

Approach:

- Use `rusqlite` directly; no ORM or repository layer.
- Create tables for projection metadata, nodes, services, gateway routes, DNS
  records, and projection statuses.
- Rebuild by replacing projection tables inside one transaction.
- Treat `applied_facts` as optional bookkeeping only; tests should delete the
  DB and prove output equality from facts.

Test scenarios:

- Empty state creates a valid database with empty tables.
- Non-empty state persists and can be queried back into the same state.
- Rebuilding twice is idempotent.
- Deleting the DB and rebuilding from the same facts yields identical rows.
- A failed write transaction does not leave mixed old/new projection rows.
- A simulated slow or stuck SQLite write respects the projection deadline and
  returns a structured timeout/failure instead of waiting indefinitely.
- Projection status rows do not persist raw rejected fact metadata for
  unauthorized, unverified, or cross-island candidates.

Verification:

- `cd MVP && cargo test -p mvp-projection`

### U4: Write Atomic Gateway And DNS Snapshots

Files:

- Add `MVP/projection/src/snapshot.rs`
- Add tests in `MVP/projection/src/snapshot.rs`

Goal:

Serialize deterministic `gateway.snapshot` and `dns.snapshot` files from
projection state and replace them atomically.

Approach:

- Use readable JSON for this slice.
- Include schema version, island, source commit IDs, deterministic snapshot
  revision, and sorted route/record data.
- Write to a same-directory temp file with `tempfile`, flush, and persist.
- Derive any snapshot revision/sequence deterministically from the selected
  source commit IDs and content hashes. Do not include wall-clock generated
  fields in the bytes compared by rebuild tests.
- Add an MVP-local snapshot loader that parses the generated JSON, validates
  schema version, island, and source commit IDs, and rejects corrupt or wrong
  schema files.
- Create missing parent directories with restrictive permissions where
  supported; reject symlinked snapshot targets with a structured path error.
- Keep snapshot schema MVP-local; do not import existing gateway/DNS crates.

Test scenarios:

- Snapshot output is deterministic for shuffled input.
- Gateway snapshot includes committed routes and drain metadata.
- DNS snapshot includes committed records and stable ordering.
- A simulated write failure leaves the previous snapshot bytes untouched.
- Missing parent directories are created with the expected permissions.
- Symlinked snapshot targets are rejected.
- The MVP-local loader accepts generated snapshots and rejects corrupt,
  wrong-schema, or wrong-island snapshots.

Verification:

- `cd MVP && cargo test -p mvp-projection`

### U5: Add Projection Actor Semantics

Files:

- Add `MVP/projection/src/actor.rs`
- Add tests in `MVP/projection/src/actor.rs`

Goal:

Expose projection through a Kameo-owned actor boundary with structured status,
bounded calls, and no hidden mutation of durable truth.

Approach:

- `ProjectionActorHandle::project_once` performs a full pass and returns
  `ProjectionReport`.
- `ProjectionActorHandle::fact_hint` records a wakeup hint but does not become
  required for correctness.
- Actor-owned status includes last successful projection, last failure, ignored
  fact count, snapshot paths, and duration.
- Actor construction binds one island and one authorized projection session.
- Keep SQLite/file work bounded and visible. If implementation uses blocking
  work, delegate it outside the Kameo mailbox.

Test scenarios:

- `project_once` writes SQLite and both snapshots.
- A dropped `fact_hint` followed by `project_once` still catches up.
- A projection failure preserves last successful status and reports the new
  failure.
- Concurrent callers do not interleave two rebuilds into mixed output.
- `project_once` cannot be redirected to another island at call time.
- Slow fact-source, SQLite, or snapshot work respects projection deadlines and
  returns structured failure.

Verification:

- `cd MVP && cargo test -p mvp-projection`

### U6: Add E2E Projection Contract And Scale Proof

Files:

- Modify `MVP/e2e/Cargo.toml`
- Modify `MVP/e2e/src/main.rs`
- Add `MVP/e2e/src/projection_contract.rs`
- Modify `MVP/e2e/src/scale.rs`
- Modify `MVP/e2e/src/metrics.rs` if new metric labels are needed

Goal:

Prove `E2E-4a` projection-contract behavior and large-load logical-node
projection shape under the MVP-local harness. This does not close E2E-4.

Approach:

- Add `projection-contract` command.
- Build a small island with authorized fact writers, write node/service/route,
  gateway, and DNS facts, project them, and assert SQLite rows and snapshots.
- Delete `projections.sqlite`, rebuild, and compare outputs.
- Simulate dropped notification delivery by writing facts without a hint and
  running full projection.
- Extend scale with 200, 1,000, and 10,000 logical nodes. Each logical node
  should have a node principal, grants, node/service facts, and enough
  route/DNS facts to exercise listing, authorization, reducer, actor, SQLite,
  and snapshot paths. Record rebuild duration, snapshot duration, ignored-fact
  counts, output sizes, and projection actor deadline outcomes.

Test scenarios:

- Node join fact appears in local node projection.
- Service registration fact appears in service projection.
- Route commit fact projects into `gateway.snapshot`.
- Gateway commit fact selects the route commit used by `gateway.snapshot`.
- DNS commit fact projects into `dns.snapshot`.
- Deleting SQLite and rebuilding from facts produces the same projection.
- Dropped notification delivery does not prevent catch-up.
- Unauthorized fact write and conflicting fact write remain visible failures.
- Unauthorized or unverified facts surfaced by the fact source are ignored with
  projection status and do not reach SQLite or snapshots.
- 10,000 logical-node rebuild completes and writes snapshots without
  nondeterminism.
- Ordinary `ProjectionReport`, SQLite rows, and snapshot files do not expose raw
  rejected fact keys, authors, or hashes for unauthorized, unverified, or
  cross-island candidates.
- Snapshot loader accepts generated files and rejects corrupt replacements. It
  does not implement gateway/DNS last-good replacement yet.

Verification:

- `cd MVP && cargo run -p mvp-e2e -- projection-contract`
- `cd MVP && cargo run -p mvp-e2e -- scale`

### U7: Update Maintainer Documentation And Slice Report

Files:

- Modify `MVP/README.md`
- Modify `MVP/primitive-decisions.md`
- Add `MVP/slice-005-fact-projection.md`

Goal:

Record why the projection primitive exists, why SQLite is disposable, why
`rusqlite`/`tempfile`/`blake3` were adopted, and why `iroh-docs` integration is
deferred to a dedicated compatibility/transport slice.

Approach:

- Add a primitive decision for deterministic projections and atomic snapshots.
- Update the existing immutable fact-set entry with payload/listing evidence.
- Record the iroh-docs Rust-version/API decision without weakening the final
  MVP requirement.
- Include local metrics from the projection contract and scale run.

Verification:

- Documentation links to this plan and to implementation evidence.
- Slice report includes commands run and metrics file paths.

## End-To-End Proof Matrix

This slice should leave these checks available:

```text
cd MVP && cargo test -p mvp-bus
cd MVP && cargo test -p mvp-projection
cd MVP && cargo run -p mvp-e2e -- projection-contract
cd MVP && cargo run -p mvp-e2e -- scale
cd MVP && just test
```

The final `just test` should include the new projection crate and E2E command
from inside `MVP/`, not the root repo `justfile`.

## Semantic-Leverage Check

Business rule:

> A durable route/gateway/DNS commit becomes the gateway/DNS serving view, and
> that view can be rebuilt from facts if local query state disappears.

The implementation should make this rule visible as:

- write route, gateway, and DNS commit facts,
- run `ProjectionActorHandle::project_once`,
- assert `gateway.snapshot` and `dns.snapshot`,
- delete `projections.sqlite`,
- run `project_once` again,
- assert the same snapshots.

The E2E test should not need to script transport, gap repair, mutable SQL head
updates, or gateway internals. If it does, the projection primitive is too weak.

## Risks And Review Focus

- SQLite accidentally becomes authority through mutable head rows.
- Reducer output depends on input iteration order.
- Snapshot writes can replace last good files with partial bytes.
- Fact payload listing creates cross-island read leakage.
- Actor mailbox becomes blocked by SQLite/file work under load.
- iroh-docs deferral turns into a parallel fact model instead of a backend seam.
- Scale proof measures only small fixtures and misses 10,000 logical-node
  behavior.

Review should include correctness, test coverage, maintainability, project
standards, reliability/failure behavior, security/authorization, and
performance.

## Follow-Up After This Slice

- Plan the iroh-docs adapter/toolchain slice against the fact-store contract.
- Decide whether `MVP/` raises Rust version to 1.91 for current iroh crates or
  pins older iroh/iroh-docs versions temporarily.
- Add docs-backed service registry facts on top of the projection reducer.
- Add gateway/DNS process-role snapshot loading and last-good serving proof.
- Use projected route commits in the deploy commit-before-drain slice.
