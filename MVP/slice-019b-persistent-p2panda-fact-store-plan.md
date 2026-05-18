---
title: Slice 019b Persistent p2panda Fact Store Plan
status: active
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution.md
  - MVP/design-notes/p2panda-substitution-audit.md
  - MVP/slice-018b-p2panda-fact-substrate.md
  - MVP/slice-018c-p2panda-deploy-restart-recovery.md
  - docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md
  - docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md
  - docs/solutions/performance-issues/machine-add-timeout-tests-2026-05-10.md
  - docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md
  - docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md
  - docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md
  - docs/solutions/performance-issues/machine-add-timeout-tests-2026-05-10.md
external:
  - https://docs.rs/p2panda-store/latest/p2panda_store/
  - https://docs.rs/p2panda-store/latest/p2panda_store/sqlite/store/struct.SqliteStore.html
---

# Slice 019b Persistent p2panda Fact Store Plan

## Problem Frame

Slice 018b introduced `mvp-p2panda-facts` as the preferred fact substrate
adapter, and Slice 018c proved deploy restart recovery over exported/imported
p2panda operations. Slice 019a then audited the remaining custom substrate and
made the next boundary explicit: do not build ACME or more product behavior on
an in-memory p2panda fact role.

This slice should make p2panda fact truth survive process restart. The target
is not a production network sync protocol yet. The target is a persistent local
operation store, rebuildable Ployz indexes, and one process-role E2E proof that
replaces the custom JSON/blob `ProcessFactSource` path with the p2panda fact
store.

The central invariant:

```text
p2panda operation log is durable truth
derived Ployz indexes are disposable
projection SQLite is disposable
serving snapshots remain last-good data-plane state
```

## Requirements Trace

- `VISION.md`: the daemon/coordinator is disposable; data-plane serving and
  existing workloads must survive control-plane failures.
- `MVP/overall-plan.md`: the next implementation/proof slice is persistent
  p2panda fact store and restartable fact role before ACME.
- `MVP/architecture.md`: durable facts are now signed p2panda operations behind
  `FactSource`, not iroh-docs local-view work.
- `MVP/e2e-proof-plan.md`: E2E must prove serving continuity, projection
  rebuild, and restart behavior through real process boundaries.
- `MVP/primitive-decisions.md`: `FactSource` remains the projection seam;
  p2panda owns signed operation envelopes, ingestion validation, and local
  persistence below that seam.
- `MVP/design-notes/p2panda-substitution-audit.md`: persistent p2panda storage
  is the next deletion path for `ProcessFactSource`, bus-backed fact fixtures,
  and the large iroh-docs local-view wrapper.
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`:
  failed rebuild/restart observations must attach uncertainty to status without
  rewriting durable fact truth.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`:
  authority-bearing restart inputs, including trusted author keys, must be
  validated before mutation.
- `docs/solutions/performance-issues/machine-add-timeout-tests-2026-05-10.md`:
  process-role restart tests should use operation-scoped short policies instead
  of sleeping on production deadlines.
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`:
  durable p2panda operations are truth; derived indexes, projections, rebuild
  health, and serving freshness are observations/status.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`:
  restart inputs and trusted-author bootstrap need validation before the role
  mutates durable fact state.
- `docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md`:
  keep local process restart/write lanes distinct from peer RPC paths.
- `docs/solutions/performance-issues/machine-add-timeout-tests-2026-05-10.md`:
  restart and timeout tests need short operation-scoped policies, not
  production-duration sleeps.

## Scope

In scope:

- Add SQLite-backed p2panda storage to `mvp-p2panda-facts`.
- Keep memory-backed p2panda storage for fast unit tests and deterministic
  harnesses.
- Rebuild all Ployz indexes from stored p2panda operations when a persistent
  store opens.
- Preserve import/write/read authorization semantics, trusted author-key
  binding, duplicate idempotency, conflict candidates, out-of-order errors,
  unverified candidates, and exact identity-based payload reads.
- Add a persistent p2panda process-role proof in `mvp-e2e`.
- Replace at least one `ProcessFactSource` serving path with the persistent
  p2panda source.
- Update maintainer docs and E2E proof status.

Out of scope:

- p2panda network sync.
- p2panda-auth membership.
- ACME.
- Real iroh transport for the p2panda fact store.
- Deleting every old fact fixture.
- Changing projection reducer semantics.
- Existing `crates/` integration.
- Replacing PloyzBus.

## Crate Scout

Checked for this plan:

- `p2panda-store` 0.5.2 exposes `SqliteStore` behind the `sqlite` feature,
  plus `create_database`, `connection_pool`, and `run_pending_migrations` in
  its SQLite module. It implements the same `OperationStore` and `LogStore`
  traits as `MemoryStore`.
- `p2panda-store` `LogStore` exposes `get_log_heights`, `get_raw_log`,
  `latest_operation`, and related log APIs. These are enough to rebuild Ployz
  indexes by enumerating authors under each Ployz island log id and replaying
  raw operations in sequence.
- `p2panda-store` queries logs by `LogId`; it does not provide a Ployz island
  discovery API. Rebuild should therefore take the known island set from role
  config or explicit persistent bootstrap metadata, not try to reverse
  p2panda's internal log-id hash.
- The p2panda store docs emphasize that storage and validation are separate:
  the application must validate operations before persistence or while
  rebuilding derived state. This matches the existing `p2panda-stream`
  ingestion path.
- `p2panda-sync` remains deferred. It is useful only after two durable stores
  exist to sync.

Relevant primary sources:

- `p2panda-store`: <https://docs.rs/crate/p2panda-store/latest>
- `p2panda-stream`: <https://docs.rs/p2panda-stream/latest/p2panda_stream/>
- `p2panda-core`: <https://docs.rs/crate/p2panda-core/latest>

Decision:

- Enable `p2panda-store/sqlite` in `MVP/p2panda-facts/Cargo.toml`.
- Keep using p2panda's `OperationStore`/`LogStore` traits internally rather
  than inventing a second storage trait unless the generic bounds become
  unreadable during implementation.
- Add a small Ployz-facing constructor for persistent storage; do not expose
  `sqlx` or p2panda SQLite pool types through business crates.

## Design Decisions

### One Store Type, Two Backends

`PandaFactStore` should stop being hard-wired to `MemoryStore`.

Implementation should prefer a small backend enum or a generic internal helper
that allows:

```text
PandaFactStore::memory(authorizer)
PandaFactStore::open_sqlite(path, authorizer).await
```

The public `PandaFactStore::new(authorizer)` can remain as a compatibility
alias for memory. The persistent constructor should:

1. create/open the SQLite database file,
2. run p2panda migrations,
3. load the p2panda `SqliteStore`,
4. rebuild Ployz indexes from operations,
5. return a `PandaFactStore` ready to serve `FactSource` reads.

Do not make reducers or E2E scenarios know which backend is in use.

### Rebuild Ployz Indexes From Operations

The current p2panda adapter keeps derived fields:

- `fact_index`,
- `operations`,
- `operation_hashes`,
- `facts`,
- `facts_by_identity`,
- `payloads`,
- `trusted_author_keys`.

After this slice, every field except trusted author keys must be rebuildable
from p2panda operations. The rebuild path should enumerate known island log ids
and authors via p2panda `LogStore` APIs, read raw operations in sequence, decode
headers, validate operation/body hash, derive metadata, and rebuild the same
candidate/payload indexes.

Trusted author keys are authority data, not operation-log data. For this slice,
the process role should persist or deterministically bootstrap known
`(island, principal) -> p2panda public key` bindings from explicit role config
or a small harness file. Do not infer trust from the operation extension
metadata. A restarted role that creates a fresh p2panda key for an existing
Ployz principal should fail with `AuthorKeyMismatch`, not silently fork
authority.

Execution-time unknown:

- p2panda `get_log_heights(log_id)` requires the Ployz log id. If the store
  only needs the `prod` island in this slice, start with explicit known-island
  rebuild. If multi-island rebuild is needed, add a tiny sidecar index of known
  island ids in the same p2panda-facts directory, but keep it clearly separate
  from fact truth.

### Import And Write Stay Validating Paths

Persistent writes and imports must still go through `p2panda-stream` ingestion.
Do not write rows directly to SQLite for convenience.

Rebuild also needs validation. If a stored operation is malformed, missing
payload, unauthorized for its author, or untrusted for its claimed principal,
it should not poison the store. The projection-facing result should match the
current contract:

- valid and authorized -> `Verified` or `Conflict`,
- revoked author -> `Unverified`,
- unreadable session -> `Unauthorized`,
- missing payload -> candidate can exist but payload read returns absent,
- malformed operation -> skipped with visible rebuild status or structured
  store error.

### Process Role Proof Targets The Serving Projection Path

The least risky E2E target is the existing serving process-role scenario:

- `MVP/e2e/src/process_role_serving_contract.rs`
- `MVP/e2e/src/process_role_harness.rs`
- `MVP/e2e/src/process_fact_source.rs`

That path already proves coordinator death, serving last-good state, projection
rebuild, remote injected serving commits, serving-role restart, and local
mutation failure after coordinator death. Add a new sibling p2panda scenario
first, while keeping `process-role-serving-contract` green. Do not overwrite the
old scenario until the p2panda path proves equivalent behavior.

Preferred scenario name:

```text
p2panda-process-role-serving-contract
```

This avoids silently weakening the old proof while the new role is introduced.
Once the new scenario passes in `all`, mark `ProcessFactSource` as legacy or
remove it from the serving process-role path.

### Fact Role Restart Is Separate From Coordinator Restart

The proof must kill/reopen the fact store independently of the local
coordinator object/process.

Minimum shape:

```text
start serving/projection role with persistent p2panda fact source
start coordinator role with same p2panda store path and writer authority
coordinator writes serving commit
projection reloads serving snapshot
kill coordinator
serving still answers from last-good
drop/reopen fact store or restart fact-role process
write/import another already-authorized remote serving commit
projection rebuilds from reopened p2panda operations
serving reloads new snapshot
delete projection SQLite
projection rebuilds from reopened p2panda operation log
serving survives during rebuild
```

The simplest first implementation can make the coordinator and serving roles
open the same persistent p2panda store path directly. A separate fact-store
process role is still the intended proof target if the harness shape stays
small. If a dedicated fact-store role adds too much IPC surface for this slice,
prove store reopen across two OS process roles and record the dedicated role as
the next process-boundary refinement. Do not ship the slice with only an
in-process reopen proof.

## Implementation Units

### U1: Persistent Backend Constructor And Cargo Feature

Files:

- Modify `MVP/p2panda-facts/Cargo.toml`
- Modify `MVP/p2panda-facts/src/lib.rs`

Approach:

- Enable `p2panda-store`'s `sqlite` feature.
- Add a persistent constructor such as `PandaFactStore::open_sqlite`.
- Keep `PandaFactStore::new` memory-backed.
- Keep p2panda SQLite setup private to `mvp-p2panda-facts`.
- Preserve existing public write/import/export/read methods.

Test scenarios:

- Memory constructor behavior remains unchanged.
- Persistent constructor creates a missing database and runs migrations.
- Reopening an empty persistent store returns no candidates.
- Store errors surface as structured `PandaFactError::Store`, not stringly
  panics.

Verification:

- `cd MVP && cargo test -p mvp-p2panda-facts --lib`
- `cd MVP && cargo clippy -p mvp-p2panda-facts --all-targets -- -D warnings`

### U2: Derived Index Rebuild From p2panda Operations

Files:

- Modify `MVP/p2panda-facts/src/lib.rs`

Approach:

- Extract current record/index logic so normal writes, imports, and rebuild
  use one path.
- Add a rebuild function that walks stored p2panda logs for known island ids
  and authors, validates operations, and reconstructs derived indexes.
- Make payload storage derived from operation bodies; do not keep a second
  durable payload store.
- Keep trusted author-key bindings explicit and supplied before or during
  rebuild.

Test scenarios:

- Write several facts, close/drop store, reopen, list the same candidates.
- Reopen preserves duplicate idempotency.
- Reopen preserves same-key/different-content conflict candidates.
- Reopen preserves exact identity-based payload reads.
- Reopen with revoked author marks candidates `Unverified` or denies payloads
  according to the existing contract.
- Rebuild handles missing payloads without serving stale bytes.

Verification:

- `cd MVP && cargo test -p mvp-p2panda-facts --lib`

### U3: Persistent Projection Contract

Files:

- Modify `MVP/e2e/src/p2panda_fact_source_contract.rs`
- Possibly modify `MVP/e2e/src/main.rs` only if a new scenario is clearer than
  extending the existing one.

Approach:

- Extend the p2panda fact-source contract to use a persistent store path.
- Preserve the existing memory/import proof if it still carries useful
  behavior, but add a reopen/rebuild section that proves projection reads from
  the reopened operation log.
- Delete projection SQLite and prove it rebuilds from the reopened p2panda
  operation log, not from in-memory indexes.

Test scenarios:

- Projection state before and after p2panda store reopen is identical.
- Gateway/DNS snapshot bytes are regenerated after projection SQLite deletion.
- Conflict candidate count survives reopen.
- Export/import still works after reopen.

Verification:

- `cd MVP && cargo run -p mvp-e2e -- p2panda-fact-source-contract`

### U4: Process-Role p2panda Serving Proof

Files:

- Modify `MVP/e2e/src/process_role_harness.rs`
- Add `MVP/e2e/src/p2panda_process_role_serving_contract.rs`
- Modify `MVP/e2e/src/main.rs`
- Possibly keep `MVP/e2e/src/process_fact_source.rs` as legacy fixture for
  older scenarios.

Approach:

- Add a p2panda-backed fact writer/source path to the process-role harness.
- Keep the old `process-role-serving-contract` until the new p2panda scenario
  proves equivalent behavior.
- The new scenario should reuse the existing Unix-socket role harness patterns,
  cleanup registry, status requests, projection rebuild request/await flow, and
  serving queries.
- Add explicit author-key trust bootstrap for local coordinator and remote
  injector authors, and make the p2panda author keys stable across coordinator,
  injector, and fact-store restart. Do not trust claimed author metadata from
  imported operations.
- Make the scenario restart or reopen the p2panda fact store while serving
  continues from last-good state.

Test scenarios:

- Coordinator writes a serving commit through p2panda facts.
- Serving/projection role loads gateway/DNS state from p2panda-backed
  projection.
- Killing the coordinator leaves serving queries green.
- Reopening/restarting the p2panda fact store preserves fact truth.
- Reopening/restarting with a different key for a trusted principal fails
  loudly instead of writing a second trusted author identity.
- Remote already-authorized serving commit is accepted after fact-store reopen.
- Projection SQLite deletion/rebuild works from reopened p2panda operation log.
- Serving continues to answer during projection rebuild.
- Local mutation through killed coordinator still fails visibly.

Verification:

- `cd MVP && cargo run -p mvp-e2e -- p2panda-process-role-serving-contract`
- `cd MVP && cargo run -p mvp-e2e -- process-role-serving-contract`

### U5: Retire Or Fence Custom ProcessFactSource Usage

Files:

- Modify `MVP/e2e/src/process_fact_source.rs`
- Modify `MVP/e2e/src/process_role_harness.rs`
- Modify `MVP/primitive-decisions.md`
- Modify `MVP/e2e-proof-plan.md`
- Add `MVP/slice-019b-persistent-p2panda-fact-store.md`

Approach:

- If all serving process-role behavior is covered by the new p2panda scenario,
  remove `ProcessFactSource` from that path.
- If other scenarios still depend on `ProcessFactSource`, mark it clearly as a
  legacy fixture and add a decision note forbidding new users.
- Record what changed in the proof map and primitive decisions.
- Include semantic leverage evidence: the new p2panda role should remove or
  fence custom JSON/blob fact persistence rather than add a second durable fact
  path forever.

Test scenarios:

- `mvp-e2e -- all` still includes both old and new paths as appropriate.
- No new product-proof scenario writes fact truth through `ProcessFactSource`.

Verification:

- `cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all`
- `git diff --check`

## Risks And Guardrails

- **Trust bootstrap drift:** do not rebuild trusted principal/key bindings from
  claimed operation metadata. Treat missing trusted keys as visible untrusted
  operations.
- **Silent stale state:** serving last-good state is acceptable only with
  freshness/status metadata. A failed fact-store reopen or projection rebuild
  must have an audience in role status.
- **Async seepage:** do not turn reducers or `FactSource` into async IPC during
  this slice. Persistent stores should do async rebuild/open work, then expose
  synchronous derived-index reads.
- **Process-role sprawl:** if adding a full fact-store RPC surface starts
  expanding beyond write/import/status/shutdown/reopen proof, split it and keep
  the first process-boundary proof smaller.
- **Cargo feature churn:** enabling `p2panda-store/sqlite` brings `sqlx`. Keep
  it isolated to `MVP/` and do not change the root workspace.
- **Test runtime:** keep restart waits operation-scoped and short; do not add
  sleeps that hide ordering bugs or grow `mvp-e2e -- all`.

## Review And Simplification Plan

Run a simplification pass after U2/U3 pass and before U4 expands the process
harness. Focus on:

- avoiding two parallel p2panda store representations,
- keeping rebuild/index code readable,
- not leaking p2panda SQLite types into business crates,
- reducing repeated writer/importer setup in E2E.

Run code review with subagents after U4/U5:

- correctness: rebuild and import semantics,
- reliability: restart/failure visibility,
- security: author-key trust and read/write grants,
- performance: rebuild scans and projection hot paths,
- maintainability: whether `ProcessFactSource` is actually fenced or just
  duplicated.

Do not spend review budget on polishing placeholder process IPC unless it
affects the persistent fact-store proof.

## Verification Gate

Required before shipping the slice:

```text
cd MVP && cargo test -p mvp-p2panda-facts --lib
cd MVP && cargo clippy -p mvp-p2panda-facts -p mvp-e2e --all-targets -- -D warnings
cd MVP && cargo run -p mvp-e2e -- p2panda-fact-source-contract
cd MVP && cargo run -p mvp-e2e -- deploy-restart-recovery-contract
cd MVP && cargo run -p mvp-e2e -- p2panda-process-role-serving-contract
cd MVP && cargo run -p mvp-e2e -- process-role-serving-contract
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
git diff --check
```

## Exit Criteria

The slice is complete when:

- a p2panda SQLite-backed `PandaFactStore` can be opened, written, dropped, and
  reopened;
- Ployz indexes rebuild from p2panda operations, not from in-memory state;
- existing p2panda write/import/read semantics still pass;
- projection can rebuild SQLite and gateway/DNS snapshots from a reopened
  p2panda operation log;
- at least one process-role E2E proves serving continuity while coordinator and
  fact-store fates are separated;
- old `ProcessFactSource` usage is removed from the serving process-role path
  or explicitly fenced as a legacy fixture with no new users;
- maintainer docs name p2panda persistence as the durable fact direction and
  record remaining sync/auth follow-up.
