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
- This plan assumes `mvp-routing` remains the owner of serving commit facts and
  projection catch-up proof.
- This plan assumes the current branch's uncommitted Slice 017 projection work
  is either completed or reverted before implementation begins. The deploy
  recovery slice should start from a clean committed boundary.

## Requirements Trace

- `VISION.md`: the daemon is disposable; the data plane outlives the control
  plane.
- `MVP/overall-plan.md`: killing the command/coordinator role must stop new
  local mutations but not serving, DNS, WireGuard, or existing workloads.
- `MVP/architecture.md`: route cutover is a durable fact, and drain is a
  consequence of that fact.
- `MVP/e2e-proof-plan.md` E2E-7: kill daemon after phase commit before drain,
  restart, rebuild projection, and resume drain.
- `MVP/primitive-decisions.md`: deploy participant cleanup before commit still
  needs a clearer ABI; this slice should document the post-commit drain/stop
  ABI it relies on.
- `MVP/design-notes/phased-command.md`: do not add `mvp-commands` until three
  or more commands repeat phase/resume/compensation logic.
- `MVP/slice-010-deploy-commit-drain-plan.md`: preserve the commit-before-drain
  invariant and cleanup-pending status semantics.

## Scope

In scope:

- Add deploy-owned durable facts under `/facts/deploy/...` for enough state to
  reconstruct post-serving-commit cleanup after coordinator death.
- Add a narrow deploy fact store boundary over the existing fact substrate.
- Record visible nodes at decision time in durable deploy state.
- Record the final phase commit and associated serving commit id before
  returning a pending-cleanup handle.
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

- Full pre-commit crash adoption/cleanup for candidates that were prepared or
  started before any irreversible commit. This remains a separate deploy
  participant ABI slice.
- Automatic rollback after irreversible phase commit.
- General `mvp-commands` / `PhasedCommand`.
- Temporal/Cadence/Restate-style activity replay.
- Real Docker/ZFS runtime operations.
- Real distributed PloyzBus over iroh streams.
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

Decision for this slice:

- Add no workflow/runtime dependency.
- Keep recovery as explicit deploy facts plus explicit resume code.
- Copy the durable-phase lesson from workflow engines, not their replay model.

## Design Decisions

### Deploy Facts Are Deploy-Owned

Do not put deploy phase facts into `ProjectionFactPayload` unless serving or
operator status projection actually needs them. `mvp-deploy` already depends on
`mvp-projection`; making projection depend on deploy would invert the current
crate boundary.

Preferred shape:

```text
MVP/deploy/src/facts.rs
  DeployFactPayload
  DeployPlanFact
  DeployPhaseCommitFact
  DeployCleanupStartedFact
  DeployCleanupDoneFact
  DeployFactStore
  BusDeployFactStore
```

The recovery path can read deploy facts directly through a narrow fact store.
Projection remains responsible for serving/gateway/DNS snapshots, not for
owning deploy orchestration state.

### Fact Keys

Use immutable command facts:

```text
/facts/deploy/<deploy_id>/plan
/facts/deploy/<deploy_id>/phase/<phase_id>/commit
/facts/deploy/<deploy_id>/cleanup/started/<epoch>
/facts/deploy/<deploy_id>/cleanup/done/<epoch>
```

The plan fact carries the submitted manifest and the visible nodes accepted at
decision time. A duplicate or conflicting plan for the same `deploy_id` is a
command conflict, not something the operator should manually pick.

The phase commit fact carries:

- deploy id,
- phase id,
- phase policy,
- visible nodes at decision time,
- optional serving commit id,
- irreversible marker.

The serving commit payload itself remains the existing `ServingCommitFact`
written by `mvp-routing`.

### Recovery State

Recovery should only rebuild a cleanup continuation when all of these are true:

- plan fact exists,
- final serving phase commit fact exists,
- serving commit fact exists,
- cleanup done fact does not exist,
- projection catch-up proof matches the serving commit.

If the plan exists but no serving commit exists, this slice should return a
structured `RecoveryState::PreCommitIncomplete` and stop. It should not pretend
to clean up prepared candidates until the participant ABI for pre-commit
cleanup is explicit.

### Post-Commit Participant ABI

Post-commit drain/stop requests must be idempotent for a `(deploy_id, backend)`
pair.

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

### Unit 1: Deploy Durable Fact Model

Files:

- `MVP/deploy/src/facts.rs`
- `MVP/deploy/src/domain.rs`
- `MVP/deploy/src/error.rs`
- `MVP/deploy/src/lib.rs`
- `MVP/deploy/src/tests.rs`

Work:

- Add deploy fact payload structs and fact key constructors.
- Add a narrow fact store boundary for writing and reading deploy facts.
- Add `BusDeployFactStore` over `BusActorHandle` fact APIs.
- Add structured errors for duplicate plan, conflicting phase commit, missing
  recovery facts, and malformed deploy fact payloads.
- Keep serving commit fact writes in `mvp-routing`.

Tests:

- Plan fact key is stable and deploy-id scoped.
- Duplicate plan with the same payload is accepted as already-present.
- Conflicting plan for the same deploy id returns structured conflict.
- Phase commit fact can be read back and decoded.
- Malformed deploy fact payload returns structured recovery error.

### Unit 2: Durable Execute Until Commit

Files:

- `MVP/deploy/src/coordinator.rs`
- `MVP/deploy/src/state_machine.rs`
- `MVP/deploy/src/facts.rs`
- `MVP/deploy/src/tests.rs`

Work:

- Add a durable execution path that writes the plan/decision fact before the
  first participant mutation.
- Preserve existing preflight behavior: capacity is checked before mutation,
  visible nodes are recorded, and missing planned nodes fail before mutation.
- Write phase commit facts after each successful phase commit.
- Write the final serving phase commit fact with the serving commit id after
  `mvp-routing::write_serving_commit` succeeds.
- Keep the existing non-durable canary path if it remains useful for focused
  tests, but avoid two divergent implementations of deploy ordering.

Tests:

- Capacity failure writes no deploy plan/phase/serving facts.
- Prepare/start failure before commit writes plan evidence but no serving commit.
- Serving commit success writes plan, phase commit, and serving commit facts in
  order.
- Drain/stop requests still do not occur before projection catch-up.

### Unit 3: Resume Pending Cleanup

Files:

- `MVP/deploy/src/coordinator.rs`
- `MVP/deploy/src/facts.rs`
- `MVP/deploy/src/state_machine.rs`
- `MVP/deploy/src/tests.rs`

Work:

- Add a recovery entry point, such as `recover_pending_cleanup(deploy_id, ...)`.
- Rebuild `PendingCleanup` from durable deploy facts and the current serving
  commit plan.
- Require `ProjectionCatchUp` before calling the existing cleanup path.
- Treat cleanup-done fact as idempotent success/no-op.
- Return structured cleanup-pending status when drain/stop responders are absent
  after restart.

Tests:

- Recovery refuses to stop old backends without projection catch-up.
- Recovery after serving commit drains and stops old backends exactly after
  projection proof.
- Recovery with missing plan returns a structured missing-fact error.
- Recovery with plan but no serving commit returns `PreCommitIncomplete`.
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
- HTTP/DNS or serving actor keeps last-good answers while the coordinator is
  absent.
- Existing `deploy-commit-drain-contract` remains green.

Metrics:

- deploy fact write duration,
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
  preflight -> plan fact -> phase side effects -> phase commit fact -> serving
  commit -> projection proof -> drain -> stop -> cleanup done.
- Whether the recovery path reads as:
  read facts -> verify serving commit/projection -> cleanup.
- Whether tests assert product invariants instead of object lifetime details.

The goal is not to minimize line count at all costs. The goal is that restart
recovery is visible as deploy semantics, not a new framework hidden under the
coordinator.

## Review Risks

- Accidentally rebuilding a manual event log. Deploy facts should be immutable
  checkpoints, not a global ordered stream.
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
3. Durable execute-until-commit path.
4. Recovery path and focused tests.
5. E2E deploy restart recovery contract.
6. Documentation/report updates.
7. Simplification pass.
8. Review-fix follow-up if review catches invariant bugs.
