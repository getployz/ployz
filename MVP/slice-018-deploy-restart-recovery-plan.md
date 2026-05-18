---
title: Slice 018 Deploy Restart Recovery Plan
status: planned
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/phased-command.md
  - MVP/slice-010-deploy-commit-drain-plan.md
  - MVP/slice-017-graceful-machine-remove-plan.md
---

# Slice 018 Deploy Restart Recovery Plan

## Problem Frame

The MVP has already proved the central deploy invariant while one coordinator
object remains alive:

```text
serving commit is durable before drain
drain is a consequence of projection catch-up
cleanup failure after commit is visible recoverable status
```

That is necessary but not enough. The strategy map says the coordinator daemon
is disposable, and `MVP/e2e-proof-plan.md` still lists deploy crash/restart
around commit and drain as an open E2E-7 gap.

The next proof after graceful machine remove should be:

```text
Kill the deploy coordinator after the local serving commit is durable but
before drain starts; a fresh coordinator reconstructs pending cleanup from
durable deploy facts plus projection evidence, drains/stops old backends only
after projection catch-up, and serving/mesh steady state continues while the
coordinator is absent.
```

This slice should not port the old deploy coordinator and should not introduce
a general workflow engine. It should make the current deploy canary durable
across the one crash point that matters most: after route cutover exists and
before destructive cleanup.

## Preconditions

- Slice 017 should ship first. Graceful machine remove is the next active
  product proof, and it will finish extracting route/serving commit semantics
  out of deploy ownership.
- The p2panda substitution investigation in
  `MVP/slice-018a-p2panda-substitution-investigation-plan.md` is complete and
  recommends adopting `p2panda-core`, `p2panda-store`, and `p2panda-stream`
  behind `FactSource`. Add that p2panda-backed fact substrate before
  implementing this deploy recovery plan, then revise this plan around that
  boundary.
- This plan assumes `mvp-routing` remains the owner of serving commit facts and
  projection catch-up proof.
- The deploy recovery slice should start from a clean committed Slice 017
  boundary.

## Requirements Trace

- `VISION.md`: the daemon is disposable; the data plane outlives the control
  plane.
- `MVP/overall-plan.md`: killing the command/coordinator role must stop new
  local mutations but not serving, DNS, WireGuard, or existing workloads.
- `MVP/architecture.md`: route cutover is a durable fact, and drain is a
  consequence of that fact.
- `MVP/e2e-proof-plan.md` E2E-7: kill the coordinator after the serving
  phase's `ServingCommitFact` is durable and before drain, restart, rebuild
  projection, and resume drain.
- `MVP/primitive-decisions.md`: deploy participant cleanup before commit still
  needs a clearer ABI; this slice should document the post-commit drain/stop
  ABI it relies on.
- `MVP/design-notes/phased-command.md`: do not add `mvp-commands` until three
  or more commands repeat phase/resume/compensation logic.
- `MVP/slice-010-deploy-commit-drain-plan.md`: preserve the commit-before-drain
  invariant and cleanup-pending status semantics.

## Scope

In scope:

- Add deploy-owned durable facts under `/facts/deploy/...` for the deploy
  decision and cleanup completion.
- Use the existing serving commit fact as the durable cutover boundary. Recovery
  must not require a second post-serving deploy fact, because the crash window
  immediately after `ServingCommitFact` is the whole point of the slice.
- Treat "coordinator restart" literally. The killed role is the deploy
  coordinator/operator-command role. The docs/fact-sync, projection, serving,
  and mesh roles may keep running, because that is the daemon-fate-separation
  model the architecture is trying to prove.
- Add a narrow deploy fact read/write boundary over the fact substrate only if
  implementation has two current consumers, such as the bus-backed focused tests
  and docs-backed E2E proof.
- Keep deploy and serving facts on one fact substrate in the restart E2E. A
  proof where deploy facts live in docs but serving facts live only in the
  in-memory bus is not a restart-recovery proof.
- Record visible nodes at decision time in durable deploy state.
- Reconstruct pending cleanup from durable deploy facts plus the existing
  `ServingCommitFact`.
- Require `ProjectionCatchUp` after restart before drain/stop.
- Make drain/stop participant requests explicitly idempotent for the same
  deploy id and backend.
- Return `CleanupPending` after restart with the same structured audience as
  the live path when drain/stop responders are unavailable.
- Add an E2E scenario named `deploy-restart-recovery-contract`.
- Update `MVP/e2e-proof-plan.md` and `MVP/primitive-decisions.md` with the
  proof status and participant ABI decision after implementation.
- Keep all changes self-contained under `MVP/`.

Out of scope:

- Any crash state before the serving commit exists, including prepared/started
  candidates and irreversible phase commits without a serving commit. This
  returns structured `PreCommitIncomplete`/missing-commit recovery status and
  remains a separate deploy participant ABI slice.
- Automatic rollback after irreversible phase commit.
- General `mvp-commands` / `PhasedCommand`.
- Temporal/Cadence/Restate-style activity replay.
- Real Docker/ZFS runtime operations.
- Real distributed PloyzBus over iroh streams.
- Fact-store process death. If the E2E kills and recreates the docs/blobs role
  as well as the coordinator, then `IrohFactNode::persistent` becomes required;
  otherwise the slice should model docs/projection as a surviving role outside
  the killed coordinator.
- Migration into existing `crates/` code.

## Crate Scout

Checked before planning:

- `restate-sdk` provides durable handlers and workflow support, including
  journaling results to avoid re-execution on retries:
  <https://docs.rs/restate-sdk/latest/restate_sdk/>. This is the wrong shape
  for Ployz commands because the MVP explicitly rejects invisible
  activity-replay semantics. Keep the idea that phase results are durable; do
  not adopt the execution model.
- `tokio-util` exposes `CancellationToken` and related shutdown utilities:
  <https://docs.rs/tokio-util/latest/tokio_util/sync/index.html>. Useful for a
  later long-lived coordinator/process-role shutdown path, but this slice is
  about durable recovery after death, not graceful cancellation.
- `async-trait` supports async functions in trait objects:
  <https://docs.rs/async-trait>. Avoid adding it by default. Use concrete fact
  store types or generic async methods first; add `async-trait` only if the
  implementation genuinely needs dyn async dispatch and the simpler shape is
  worse.
- `iroh-docs` already exposes `Docs::persistent(path)` for redb-backed docs
  storage, and `iroh-blobs` exposes `FsStore::load(path)` for file-backed blob
  storage. The current `mvp-iroh` proof wrapper only exposes `memory()`, so a
  process-death durability proof may need a small `IrohFactNode::persistent`
  helper before the deploy E2E can honestly kill and recreate the local docs
  node.
- The current MVP lockfile resolves `iroh = 1.0.0-rc.0`, `iroh-docs = 0.99.0`,
  and `iroh-blobs = 0.101.0`. The official iroh docs still describe the same
  persistence pairing: `Docs::persistent(path)` for docs metadata plus
  `FsStore::load(path)` for blob content. `iroh-blobs` notes that a clean store
  shutdown is the reliable way to flush recent filesystem-backed writes, so a
  kill-the-docs-role proof needs a deliberately chosen crash/flush contract.

Decision for this slice:

- Add no workflow/runtime dependency.
- Keep recovery as explicit deploy facts plus explicit resume code.
- Copy the durable-phase lesson from workflow engines, not their replay model.
- Prefer the existing iroh-docs fact substrate for the E2E recovery proof.
  Bus-backed fact writes may stay as a focused unit-test adapter, but the
  restart E2E must read deploy decision, serving commit, and cleanup-done facts
  from the same surviving docs-backed substrate.
- Do not spend this slice proving docs-role crash recovery unless the
  implementation naturally needs it. The product proof is coordinator restart
  while steady-state roles continue.

## Design Decisions

### Deploy Facts Are Deploy-Owned

Do not put deploy recovery facts into `ProjectionFactPayload` unless serving or
operator status projection actually needs them. `mvp-deploy` already depends on
`mvp-projection`; making projection depend on deploy would invert the current
crate boundary.

Preferred shape:

```text
MVP/deploy/src/facts.rs
  DeployFactPayload
  DeployDecisionFact
  DeployCleanupDoneFact
  DeployFactWriter
  DeployFactReader
  BusDeployFactWriter for focused tests if still useful
  DocsDeployFactWriter for the restart E2E
```

The recovery path can read deploy facts directly through a narrow fact store or
through concrete helpers over `FactSource`. Projection remains responsible for
serving/gateway/DNS snapshots, not for owning deploy orchestration state.

`mvp-routing` still owns serving commit fact shape. This slice may need pure
helpers such as `serving_commit_fact_key` and `serving_commit_fact_payload` so
the same serving fact can be written to either the bus-backed harness or
docs-backed E2E store without making deploy depend on raw projection payload
construction.

Deploy fact readers should decode deploy payloads themselves. They may reuse a
generic fact-source/local-view byte reader, but they must not force deploy
decision and cleanup-done facts through `ProjectionFactPayload` just to make the
projection reducer understand them.

### Participant RPC And Fact Writes Are Separate

The current deploy coordinator writes serving commits through
`mvp-routing::write_serving_commit(&BusActorHandle, ...)`. That is useful for
the Slice 010 bus-backed canary, but it is the wrong dependency for the restart
proof. Slice 018 must split the coordinator dependencies:

```text
BusActorHandle
  participant RPC only: capacity, prepare, start, drain, stop

DeployFactWriter / ServingFactWriter
  durable command truth: decision, serving commit, cleanup done
```

Follow the shape already proven by `MachineFactWriter` in `mvp-machine`: inject
a fact writer into the coordinator, keep the bus for request/reply, and let the
E2E provide a docs-backed writer while focused tests may use an in-memory or
bus-backed writer.

The deploy writer boundary should stay narrow:

```text
DeployFactWriter
  write_decision(...)
  write_cleanup_done(...)
```

The writer outcome can be deploy-local and lighter than
`mvp_bus::FactWriteOutcome`: command code only needs inserted/already-present as
success and structured conflict as a branchable error. Bus-backed adapters may
translate from `FactWriteOutcome`; docs-backed adapters should translate from
the iroh-specific immutable write outcome.

Do not make the E2E pass by writing the serving commit through the in-memory bus
while decision and cleanup-done live in iroh-docs. That would leave the cutover
truth outside the recovery substrate.

### Docs-Backed Immutable Writes

The current `IrohFactDoc::write_fact_payload` returns only a content hash and
uses `doc.set_bytes(author, key, payload)`. It does not currently expose the
bus-style `Inserted` / `AlreadyPresent` / `Conflict` outcome.

For this slice, any docs-backed fact writer used by deploy recovery must enforce
the Ployz immutable fact contract before mutation:

1. Refresh/read the exact key from the local docs view.
2. If no authorized candidate exists, write the payload and return `Inserted`.
3. If an authorized candidate with the same content hash already exists, return
   `AlreadyPresent`.
4. If an authorized candidate with a different content hash exists, return a
   structured conflict before writing.
5. If replication later reveals multiple authorized candidates anyway, recovery
   treats them as conflict candidates and applies the deterministic selection
   rule described below.

This should be a small helper in `mvp-iroh` with an iroh-specific outcome type,
not a forced reuse of `mvp_bus::FactWriteOutcome`. The bus `Fact` constructor is
private, and that is a good boundary: the docs helper only needs to report the
key, content hash, author/principal, and conflict candidate metadata that deploy
can branch on.

The write preflight must inspect existing exact-key candidates by write
authority, not by the caller's read grant. Do not use
`IrohDocsFactSource::list_candidates` as the write preflight, because that API
correctly filters for projection/read access. Conflict detection for immutable
writes needs to see existing authorized writers even when the current writer
does not have read permission for those facts.

The important contract is that command entry does not knowingly overwrite a
different same-key fact.

Deploy recovery reads can use `FactSource` as the existing narrow read boundary,
but deploy facts must decode their own payload bytes. `/facts/deploy/...` keys
will classify as unsupported by projection, and that is acceptable; unsupported
classification must not force deploy facts into `ProjectionFactPayload`.

### Coordinator Death Boundary

The restart proof should separate these roles explicitly:

```text
killed:
  DeployCoordinator / operator-command role

survives:
  docs/fact-sync role
  projection actor/process role
  serving actor/process role
  mesh/data-plane role
```

The fresh coordinator must be built from its constructor plus fact readers. It
must not retain a `PendingCleanup`, participant counters, capacity results, or
other in-memory state from the killed coordinator.

If the test uses an in-process harness, the "kill" is acceptable only if the
facts live outside the coordinator object and the restarted coordinator reads
them through the fact boundary. If the test uses OS process death for the docs
role too, the plan must add `IrohFactNode::persistent` first and make the flush
contract explicit.

### Fact Keys

Use immutable command facts:

```text
/facts/deploy/<deploy_id>/decision
/facts/deploy/<deploy_id>/cleanup/done
/facts/serving/<serving_commit_id>   # existing mvp-routing serving fact
```

The decision fact carries the submitted manifest, visible nodes accepted at
decision time, the expected serving commit id, and the serving epoch used for
deterministic candidate selection. It is written after capacity preflight
succeeds and before the first participant mutation.

A duplicate decision fact with an identical payload is idempotent/already
present. A different payload for the same `deploy_id` is a structured command
conflict, not something the operator should manually pick.

That conflict rule has two phases:

- At command entry, if a visible decision fact for the same deploy id already
  exists with incompatible content, fail before mutation with a structured
  conflict that names the fact key, principal, and content hash.
- During recovery, if replication has produced multiple authorized decision
  candidates despite that preflight, select deterministically by
  `(serving_epoch desc, content_hash asc)` and surface the losers as
  superseded recovery status. Do not ask the operator to choose.

Do not add `cleanup/started`, attempt, or epoch facts in this slice. Recovery is
derived from durable decision facts plus the serving commit fact plus the
absence of cleanup done. Attempt history belongs to a later command primitive
only if multiple commands prove that need.

The serving commit payload itself remains the existing `ServingCommitFact`
owned by `mvp-routing`.

`mvp-routing` should expose pure helpers for serving fact construction and exact
read validation:

```text
serving_commit_fact_key(...)
serving_commit_fact_payload(...)
serving_commit_fact_body(...)
decode_serving_commit_fact_payload(...)
read_exact_serving_commit(...)
```

The exact reader must validate the requested serving commit id against the
decoded `ProjectionFactPayload::ServingCommit` and reject mismatched epoch,
active backends, old backends, and the rest of the serving plan. Recovery must
never ask the projection reducer for the current serving head to decide what to
resume.

### Recovery State

Recovery has two explicit states:

```text
RecoveredPendingCleanup
  rebuilt from facts, not yet allowed to drain

ProjectedPendingCleanup
  RecoveredPendingCleanup + ProjectionCatchUp proof
  the only valid input to drain/stop
```

Recovery can rebuild `RecoveredPendingCleanup` when all of these are true:

- decision fact exists,
- the exact serving commit fact named by the winning decision exists,
- cleanup done fact does not exist.

Recovery must not use the current projected serving head to infer which deploy
to resume. It should read the decision's expected `/facts/serving/<id>` fact
directly and validate that the payload matches the decision's expected serving
commit id, old backends, and epoch. Projection catch-up is evidence that local
serving outputs have caught up; it is not authority for choosing the recovery
target.

`ProjectedPendingCleanup` requires a fresh `ProjectionCatchUp` proof matching
the serving commit and remains the only path to drain/stop.

If the decision fact exists but no serving commit exists, this slice should
return a structured `RecoveryState::PreCommitIncomplete` and stop. It should
not pretend to clean up prepared candidates, even if an earlier irreversible
phase happened, until the participant ABI for pre-serving-commit cleanup is
explicit.

If the serving commit fact exists but a separate post-serving deploy checkpoint
does not, recovery must still work from the decision fact plus serving fact.
That closes the crash window where `mvp-routing::write_serving_commit` succeeds
and the coordinator dies before any later deploy-owned write can happen.

### Post-Commit Participant ABI

Post-commit drain/stop requests must be idempotent for a
`(deploy_id, cleanup_target)` pair.

The current wire requests only carry `deploy_id`, while the request subject
implies a node. This slice should remove that ambiguity by adding explicit
cleanup-target identity to the cleanup requests. For this slice, a cleanup
target is the exact `BackendEndpoint` from
`ServingCommitPlan.old_backends_to_drain`: node id plus backend address.

```text
DrainInstanceRequest { deploy_id, cleanup_target: BackendEndpoint }
StopInstanceRequest  { deploy_id, cleanup_target: BackendEndpoint }
```

After restart, the coordinator may send drain/stop again because the old process
could have died after the participant performed the side effect but before the
coordinator wrote cleanup completion. Repeating drain/stop is acceptable only
because the participant contract says these requests are idempotent.

That ABI must be recorded in `MVP/primitive-decisions.md` when the slice lands.

### No PhasedCommand Yet

This slice should not introduce `mvp-commands`.

Current count before Slice 018:

- deploy has a phase state machine and will gain deploy-specific recovery,
- ACME has advisory lease/challenge lifecycle but not a multi-phase command
  runner,
- graceful machine remove may be phase-shaped but is still explicit.

After this slice, re-count. If machine remove and another command both grow
resume-from-phase bookkeeping, plan `mvp-commands` next. Do not use Slice 018 as
an excuse to build the general primitive early.

## Implementation Units

### Unit 1: Deploy And Serving Fact Boundary

Files:

- `MVP/routing/src/lib.rs`
- `MVP/deploy/src/facts.rs`
- `MVP/deploy/src/domain.rs`
- `MVP/deploy/src/error.rs`
- `MVP/deploy/src/lib.rs`
- `MVP/iroh/src/facts.rs` if a persistent docs helper is needed for the E2E
- `MVP/deploy/src/tests.rs`

Work:

- Add deploy decision and cleanup-done fact payload structs and fact key
  constructors.
- Shape `DeployDecisionFact` around the manifest, visible nodes, expected
  serving commit id, and serving epoch. Shape `DeployCleanupDoneFact` around
  deploy id, serving commit id, cleanup targets, and serving epoch.
- Expose pure serving commit key/payload helpers from `mvp-routing` so serving
  commits can be written through the same fact substrate used by deploy facts
  in the recovery E2E.
- Split serving commit creation into pure key/payload construction plus writer
  adapters. Keep the existing bus-backed `write_serving_commit` for Slice 010
  tests if useful, but add a docs-backed writer path for Slice 018.
- Add an exact-serving-commit read helper that decodes
  `/facts/serving/<serving_commit_id>` from raw fact bytes and validates the
  expected serving id, epoch, and old backends. Do not recover by asking the
  projection reducer for the current serving head.
- Add narrow writer/reader boundaries because this slice has two current
  adapters: bus/in-memory focused tests and docs-backed E2E recovery.
  Keep the boundary specific to decision, serving commit, and cleanup-done
  facts; do not introduce a whole-store facade.
- Use `FactSource` for deploy recovery reads where it fits, but keep deploy
  payload decoding in `mvp-deploy`. Do not teach projection about deploy
  decision or cleanup-done payloads.
- Add a docs-backed immutable write helper or adapter that returns
  inserted/already-present/conflict outcomes before deploy uses iroh-docs as
  its recovery substrate. Prefer `IrohImmutableWriteOutcome` or similarly
  narrow naming over exposing bus internals.
- Bind the iroh author before exact-key read/refresh in that helper so same
  author candidates classify as verified during the preflight.
- Detect conflicts against existing authorized writers even when the current
  writer would not be allowed to read those facts through `FactSource`.
- Add a persistent `mvp-iroh` fact node helper if the E2E needs to recreate the
  docs node from disk instead of sharing an in-memory local view.
- Do not copy the machine-remove E2E's split-source shape where machine facts
  come from docs but serving facts stay bus-backed. Slice 018's restart proof
  needs deploy decision, serving commit, and cleanup-done on the same docs-backed
  substrate.
- Add structured errors for conflicting decision facts, missing recovery facts,
  malformed deploy fact payloads, and malformed serving commit payloads.
- Keep serving commit fact shape and pure key/payload helpers in `mvp-routing`.
  Durable writes should go through the injected fact writer in the restart path.
- Do not add phase commit facts, cleanup-started facts, or attempt logs in this
  slice.

Tests:

- Decision fact key is stable and deploy-id scoped.
- Duplicate decision with the same payload is accepted as already-present.
- Conflicting decision for the same deploy id returns structured conflict.
- Docs-backed immutable writes return already-present for identical bytes and
  structured conflict for different same-key bytes.
- Docs-backed immutable writes detect an existing authorized conflicting writer
  without relying on caller read permission.
- Recovery over two authorized decision candidates selects the deterministic
  winner and reports superseded candidates without operator choice.
- Cleanup-done fact uses one deterministic deploy-id-scoped key.
- Serving commit key/payload helpers round-trip through the selected fact
  source.
- Exact serving commit read refuses a different serving id, epoch, or old
  backend set.
- Malformed deploy fact payload returns structured recovery error.

### Unit 2: Durable Execute Until Serving Commit

Files:

- `MVP/deploy/src/coordinator.rs`
- `MVP/deploy/src/state_machine.rs`
- `MVP/deploy/src/facts.rs`
- `MVP/deploy/src/tests.rs`

Work:

- Add a durable execution path that writes the decision fact before the first
  participant mutation.
- Inject the deploy/serving fact writer into the durable execution path. The
  coordinator still uses the bus for participant RPC, but it must not require
  fact writes to go through `BusActorHandle`.
- Preserve existing preflight behavior: capacity is checked before mutation,
  visible nodes are recorded, and missing planned nodes fail before mutation.
- Write only the durable checkpoint needed for this proof: the decision fact
  before mutation and the serving commit fact at cutover.
- Do not require a deploy-owned fact after serving commit for recovery. If the
  implementation adds any post-serving status fact for observability, recovery
  must still succeed without it.
- Keep the existing non-durable canary path if it remains useful for focused
  tests, but avoid two divergent implementations of deploy ordering.
- Extend cleanup request payloads to include the explicit cleanup target so
  drain/stop retries are idempotent per `(deploy_id, cleanup_target)`.

Tests:

- Capacity failure writes no deploy decision/serving facts.
- Prepare/start failure before commit writes decision evidence but no serving
  commit.
- Serving commit success writes decision and serving commit facts in order.
- Drain/stop requests still do not occur before projection catch-up.
- Drain/stop requests include the cleanup target being cleaned up.

### Unit 3: Resume Pending Cleanup

Files:

- `MVP/deploy/src/coordinator.rs`
- `MVP/deploy/src/facts.rs`
- `MVP/deploy/src/state_machine.rs`
- `MVP/deploy/src/tests.rs`

Work:

- Add a recovery entry point, such as `recover_pending_cleanup(deploy_id, ...)`.
- Add a state-machine recovery constructor, such as
  `DeployStateMachine::recover_pending_cleanup(...)`, instead of replaying
  historical transitions from facts.
- Rebuild `RecoveredPendingCleanup` from durable deploy decision facts and the
  exact serving commit fact named by the winning decision.
- Require `ProjectionCatchUp` before calling the existing cleanup path.
- Treat cleanup-done fact as idempotent success/no-op.
- Return structured cleanup-pending status when drain/stop responders are absent
  after restart.
- Write cleanup-done only after drain/stop completes successfully.

Tests:

- Recovery refuses to stop old backends without projection catch-up.
- Recovery after serving commit drains and stops old backends exactly after
  projection proof.
- Recovery with missing decision fact returns a structured missing-fact error.
- Recovery with decision fact but no serving commit returns
  `PreCommitIncomplete`.
- Recovery with serving commit but no deploy-owned post-serving checkpoint still
  rebuilds pending cleanup from decision + serving facts.
- Recovery with cleanup done returns already-complete result without new
  participant RPC.
- Restarted cleanup failure returns `CleanupPending` with visible nodes and
  serving commit id.

### Unit 4: E2E Deploy Restart Recovery Contract

Files:

- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/deploy_restart_recovery_contract.rs`
- `MVP/e2e/src/process_role_harness.rs`
- `MVP/e2e-proof-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/slice-018-deploy-restart-recovery.md` after implementation

Work:

- Add `deploy-restart-recovery-contract`.
- Reuse the deploy participant fixture style from
  `MVP/e2e/src/deploy_commit_drain_contract.rs`.
- Run deploy until the serving commit is durable.
- Use one docs-backed fact source/sink for deploy decision, serving commit, and
  cleanup-done facts. Do not make this proof depend on the in-memory bus fact
  store for cutover truth.
- Make the killed coordinator role explicit in the harness output. The docs,
  projection, serving, and mesh roles should either be separate process roles or
  separately owned harness objects that are not dropped with the coordinator.
- Project the serving commit and prove old backend remains alive before drain.
- Kill/drop the coordinator before cleanup starts.
- Keep serving/mesh steady state alive through the outage using existing
  process-role harness patterns where practical.
- Start a fresh coordinator/recovery path from durable deploy facts.
- Require projection catch-up, then finish drain/stop.
- Emit structured metrics.

Required assertions:

- Visible nodes at decision time survive recovery.
- No drain/stop request occurs before projection catch-up.
- New coordinator does not re-run capacity, prepare, or start for already
  committed phases.
- Restarted coordinator drains/stops old backends after projection catch-up.
- Cleanup-pending after restart is structured and includes the serving commit id.
- The serving actor keeps last-good typed gateway and DNS answers while the
  coordinator is absent. If the implementation can reuse the wire-serving
  process harness without broadening the slice, the E2E may additionally assert
  HTTP and DNS packets; the required surface for this slice is the serving actor
  because Slice 013 already proves wire framing over the same last-good state.
- Existing `deploy-commit-drain-contract` remains green.
- The recovery path does not require any post-serving deploy-owned phase fact.

Metrics:

- deploy decision fact write duration,
- serving commit to simulated kill duration,
- coordinator outage duration,
- recovery read duration,
- projection catch-up duration,
- resumed drain duration,
- resumed stop duration,
- data-plane requests served during coordinator outage.

Tests:

- `cargo test -p mvp-deploy --lib`
- `cargo run -p mvp-e2e -- deploy-restart-recovery-contract`
- `cargo run -p mvp-e2e -- deploy-commit-drain-contract`
- `cargo run -p mvp-e2e -- wire-serving-contract`
- `MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all`

## Proof Criteria

The slice is complete when:

- A deploy can be resumed after the coordinator dies after serving commit and
  before drain.
- Recovery derives its continuation from durable facts, not in-memory
  `PendingCleanup`.
- The durable facts required for recovery are the deploy decision fact, existing
  serving commit fact, and absence of cleanup-done; no cleanup-started attempt
  log or post-serving deploy checkpoint is required.
- Drain/stop remain gated by projection catch-up after restart.
- Serving/HTTP/DNS last-good behavior continues while the coordinator is absent.
- Recovery does not re-run pre-commit participant operations.
- Cleanup-pending status after restart is as structured as the live path.
- The process or harness report makes it obvious which role died and which
  roles kept serving.
- Full MVP E2E remains within the all-scenario time budget.

## Semantic-Leverage Check

Before implementation, record:

```text
rg -n "PendingCleanup|execute_until_serving_commit|finish_cleanup|PhaseState|DeployStateMachine" MVP/deploy MVP/e2e
```

After implementation, inspect:

- Whether durable recovery added a small deploy fact boundary or leaked fact
  choreography through all deploy business logic.
- Whether the command ordering still reads as:
  preflight -> decision fact -> phase side effects -> serving commit ->
  projection proof -> drain -> stop -> cleanup done.
- Whether the recovery path reads as:
  read facts -> verify serving commit/projection -> cleanup.
- Whether tests assert product invariants instead of object lifetime details.

The goal is not to minimize line count at all costs. The goal is that restart
recovery is visible as deploy semantics, not a new framework hidden under the
coordinator.

## Review Risks

- Accidentally rebuilding a manual event log. Deploy facts should be immutable
  checkpoints, not a global ordered stream.
- Accidentally requiring a deploy-owned post-serving checkpoint for recovery.
  Recovery must cover the crash immediately after the serving fact is durable.
- Hiding pre-commit partial cleanup behind an underspecified participant
  contract. Keep that out of scope unless the ABI is explicitly written.
- Re-running prepare/start on recovery after the serving commit exists.
- Letting recovery stop old backends without fresh projection catch-up proof.
- Smuggling `PhasedCommand` into deploy before enough commands prove the shape.
- Putting deploy facts into projection and creating a dependency cycle or a
  projection catch-all.
- Making the E2E pass by killing an object while all relevant durable state is
  still just process memory. The recovery proof must use fact persistence.

## Suggested Commit Shape

1. Plan document.
2. Deploy durable fact model and focused tests.
3. Durable execute-until-serving-commit path.
4. Recovery path and focused tests.
5. E2E deploy restart recovery contract.
6. Documentation/report updates.
7. Simplification pass.
8. Review-fix follow-up if review catches invariant bugs.
