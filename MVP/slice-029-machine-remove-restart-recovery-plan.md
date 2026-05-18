---
title: Slice 029 Machine Remove Restart Recovery Plan
status: completed
created: 2026-05-18
completed: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-017-graceful-machine-remove.md
  - MVP/slice-018c-p2panda-deploy-restart-recovery.md
  - MVP/slice-027-routing-owned-serving-commit.md
  - MVP/slice-028-p2panda-machine-remove-facts.md
  - MVP/machine/src/remove.rs
  - MVP/machine-p2panda/src/lib.rs
  - MVP/e2e/src/machine_remove_contract.rs
external:
  - https://docs.rs/cano/latest/cano/
  - https://docs.rs/changeset-saga/latest/changeset_saga/
reviewed_by:
  - ce-repo-research-analyst
  - ce-learnings-researcher
  - ce-spec-flow-analyzer
---

# Slice 029 Machine Remove Restart Recovery Plan

## Problem Frame

Machine remove now writes joined-node, removal-started, tombstone, and serving
facts through one p2panda-backed source. The remaining gap is the crash point
Slice 028 deliberately deferred:

```text
serving cutover committed -> coordinator dies -> stop/tombstone not done yet
```

Today the command can continue only because `machine-remove-contract` keeps the
original in-memory `PendingMachineRemove` value and calls `finish_cleanup`. A
real coordinator restart loses that value. The durable facts have
`NodeRemovalStarted` and the serving commit, but not enough request context to
reconstruct the pending cleanup safely: `tombstone_epoch`, `visible_nodes`, the
exact intended `ServingCommitPlan`, and command completion proof are not
persisted as machine-remove command facts.

Slice 029 should add that recovery surface without turning machine remove into
a generic workflow engine. The proof should mirror deploy restart recovery:
write durable command intent, commit serving cutover, drop the coordinator,
replay p2panda operations into a fresh store, recover pending cleanup from
facts, require projection catch-up, stop workloads, tombstone, and write an
explicit cleanup-done fact so later recovery is a no-op.

## Single Proof Target

`machine-remove-contract` proves coordinator recovery after serving commit and
before stop/tombstone:

1. a machine-remove decision fact records the request context before participant
   mutation,
2. serving cutover is written durably through `mvp-routing`,
3. the original coordinator is dropped before cleanup,
4. a fresh p2panda store imports the surviving operations through trusted
   replica authority,
5. a fresh coordinator reconstructs pending cleanup from p2panda-backed facts,
6. recovery does not replay probe, prepare/drain, or serving commit writes,
7. stop still waits for `ProjectionCatchUp`,
8. tombstone writes only after stop succeeds,
9. cleanup-done makes a second recovery complete without RPC,
10. remaining data-plane traffic keeps working while the coordinator is absent.

## Requirements Trace

- `VISION.md`: `machine remove` is a north-star primitive and must complete or
  fail cleanly with visible preconditions and clear verification hooks.
- `MVP/overall-plan.md`: killing the coordinator must not kill steady-state
  data-plane behavior; already-committed state should continue through
  projection and serving roles.
- `MVP/architecture.md`: durable facts are explicit operator intent and
  lifecycle facts. Projection and live observation are evidence, not truth.
- `MVP/e2e-proof-plan.md`: E2E-5 requires graceful remove; E2E-7 requires
  crash/restart behavior; E2E-9 requires semantic leverage evidence.
- `MVP/slice-018c-p2panda-deploy-restart-recovery.md`: deploy already proves
  the durable-intent plus serving-commit plus cleanup-done recovery shape.
- `MVP/slice-028-p2panda-machine-remove-facts.md`: raw tombstone excludes a
  node from scheduling/mesh projection, but is not cleanup proof.

## Dependency Scout

Checked on 2026-05-18:

- `cano` is a Rust async workflow engine with enum states, checkpoint stores,
  crash recovery, and saga compensation. Its documented recovery model re-enters
  the last checkpointed state and re-runs that state's task, so idempotency is a
  task requirement. This is conceptually close to the future `PhasedCommand`
  note, but it would import a workflow framework before the MVP has enough
  repeated command shapes to justify one.
- `changeset-saga` provides type-state saga construction and rollback
  bookkeeping, but the crate is young and focused on generic saga execution
  rather than Ployz's signed-fact recovery semantics.
- The local deploy restart recovery path already solves the exact MVP-shaped
  problem without an external workflow dependency: explicit facts, explicit
  projection proof, and no hidden activity replay.

Decision:

- Do not add a workflow/saga dependency in this slice.
- Copy the useful idea from workflow engines only at the design level: persist
  phase/decision facts before side effects, require idempotent recovered steps,
  and make compensation/completion explicit.
- Keep `PhasedCommand` deferred. This is the second clear command with
  resume-from-phase logic, not enough to justify a generic command runner under
  the trigger in `MVP/design-notes/phased-command.md`.

## Scope

In scope:

- Machine-remove command facts in `mvp-machine`:
  - decision fact with target, removal epoch, tombstone epoch, reason, visible
    nodes, and the exact `ServingCommitPlan`,
  - cleanup-done fact with target, removal epoch, tombstone epoch, serving
    commit id/epoch, and tombstone fact evidence.
- Fact key/payload helpers and readers for those command facts.
- Recovery API that reads command facts and exact serving commit facts from a
  `FactSource`.
- Coordinator changes to write decision before participant mutation and
  cleanup-done after tombstone.
- p2panda adapter support for the new machine-remove command facts.
- E2E recovery proof in the existing `machine-remove-contract`, not a parallel
  duplicate scenario.
- Documentation and semantic-leverage accounting.

Out of scope:

- Generic `mvp-commands` / `PhasedCommand`.
- Background startup reconciliation or autonomous cleanup loops.
- Real Docker/ZFS/runtime cleanup backends.
- Kernel WireGuard mutation.
- Machine add/invite changes.
- Serving fact ownership changes outside `mvp-routing`.
- Quorum, witness acks, active-member consensus, or strict lease semantics.
- Changes outside `MVP/`.

## Key Decisions

### Decision Fact, Not Inference From Projection

Recovery should not infer request context from projection state. A
`NodeRemovalStarted` projection fact is intentionally small: node id, epoch,
and reason. The recoverable command needs more: visible-node evidence,
tombstone epoch, and exact serving plan. Add a separate machine-remove decision
fact under a machine-remove command namespace instead of overloading membership
facts.

### Cleanup-Done Fact, Not Raw Tombstone Completion

Tombstone remains membership/projection truth: the node is excluded. It is not
proof that route cutover caught up and stop completed. Add an explicit
machine-remove cleanup-done fact written only after successful stop and
tombstone. Recovery may return complete only when cleanup-done validates against
the decision and the expected tombstone fact exists in the recovered
`FactSource`. A cleanup-done payload that merely claims tombstone evidence is
not sufficient.

### Probe Before Durable Decision, Then Persist Before Mutation

The existing command probes the target before any fact write. Keep that
operator-facing behavior: if the target cannot answer the preflight probe, the
command fails before writing durable remove intent. Once the probe succeeds,
write `MachineRemoveDecision` before `NodeRemovalStarted`, participant drain,
or serving cutover. This records the recovery input before the first durable or
participant mutation while avoiding a stuck intent for a command that never got
past reachability.

### Explicit Recovery Entry Point

This slice should expose an internal recovery method, not automatic background
reconciliation. The MVP operator model is command-shaped; retry/resume can call
the recovery method later. No hidden loop should wake up and mutate durable
truth.

### Exact Serving Commit Required

Recovery must validate the exact serving commit referenced by the decision,
not the latest serving projection. If the serving commit is missing, recovery
returns pre-commit incomplete or a structured recovery error without stop or
tombstone.

### Idempotent Stop ABI

Recovered cleanup may retry `StopRemovedWorkloads`. The participant ABI should
be named as idempotent for the target/remove operation: if workloads are already
stopped for this remove, returning `Stopped` is correct. The slice does not need
a new wire payload unless implementation shows the existing target/reason pair
cannot express the idempotency contract.

## High-Level Design

This is directional guidance, not implementation code:

```text
execute_until_serving_commit(request):
  validate preconditions
  probe target
  write MachineRemoveDecision(target, epochs, visible_nodes, serving_commit)
  write NodeRemovalStarted
  request target drain/no-new-work
  write ServingCommit
  return PendingMachineRemove reconstructed from the decision

recover_pending_cleanup(source, island, session, remove_id):
  decision = read exact MachineRemoveDecision
  serving = read exact ServingCommit(decision.serving_commit)
  if serving missing:
    return PreCommitIncomplete(decision)
  if cleanup_done exists:
    validate cleanup_done + expected tombstone fact against decision
    return CleanupDone(command_result)
  return Pending(PendingMachineRemove from decision)

finish_cleanup(pending, projection_catch_up):
  require projection catch-up for pending.serving_commit
  request stop target workloads
  write NodeTombstoned
  write MachineRemoveCleanupDone
  return Removed
```

## Implementation Units

### U1. Machine Remove Command Facts

**Goal:** Add durable machine-remove decision and cleanup-done facts in
`mvp-machine`, with helpers to encode, decode, write, and read them from any
`FactSource`.

**Requirements:** Durable request context; explicit cleanup completion; no raw
tombstone-as-completion.

**Dependencies:** None.

**Files:**

- Create `MVP/machine/src/facts.rs`
- Modify `MVP/machine/src/lib.rs`
- Modify `MVP/machine/src/error.rs`
- Modify `MVP/machine/src/remove.rs` only for imports/trait wiring if needed
- Test in `MVP/machine/src/tests.rs` or the existing remove tests module

**Approach:**

- Define a stable operation identity, likely target node plus removal epoch.
  If introduced as a value, make it a newtype or typed struct because it
  participates in fact keys and recovery.
- Add a machine-remove command payload enum separate from
  `ProjectionFactPayload`.
- Add:
  - `MachineRemoveDecisionFact`,
  - `MachineRemoveCleanupDoneFact`,
  - `MachineRemoveFactWriteStatus` if needed.
- Use mostly immutable keys, for example:
  - `/facts/machine-remove/<node_id>/<removal_epoch>/decision`
  - `/facts/machine-remove/<node_id>/<removal_epoch>/cleanup/done`
- Add read helpers that mirror deploy's decision and cleanup-done readers:
  exact-key lookup, decode expected kind, reject payload/key mismatch, and
  surface conflicts as structured `MachineRemoveError`.
- Keep projection reducers unchanged. These command facts are for recovery, not
  membership projection.

**Patterns to follow:**

- `MVP/deploy/src/facts.rs`
- `MVP/deploy/src/coordinator.rs`
- `MVP/routing/src/lib.rs` exact serving commit helpers

**Test scenarios:**

- Decision fact key is deterministic for target/removal epoch.
- Decision payload round-trips and carries visible nodes plus exact serving
  plan.
- Cleanup-done payload round-trips and carries tombstone evidence.
- Decoding decision from cleanup-done payload returns a typed kind mismatch.
- Conflicting decision candidates surface a structured recovery/fact conflict.
- Unauthorized/unreadable candidates are ignored or reported consistently with
  existing `FactSource` semantics.
- Cleanup-done validation fails if serving commit id, epoch, target, or
  tombstone epoch differs from the decision.
- Cleanup-done validation fails if the expected tombstone fact is missing from
  the same recovered `FactSource`.

**Verification:** Focused `mvp-machine` tests prove the fact contract before
coordinator behavior changes.

### U2. Coordinator Recovery API

**Goal:** Extend `MachineRemoveCoordinator` so normal execution writes durable
decision/cleanup facts, and recovered coordinators can reconstruct pending
cleanup from facts.

**Requirements:** No pre-commit replay after restart; stop only after
projection catch-up; cleanup-done idempotency.

**Dependencies:** U1.

**Files:**

- Modify `MVP/machine/src/remove.rs`
- Modify `MVP/machine/src/error.rs`
- Test in existing `MVP/machine/src/remove.rs` test module or split focused
  tests if implementation creates a new module

**Approach:**

- Extend `MachineFactWriter` or introduce a narrower command-fact writer seam
  only if it keeps call sites simpler. Avoid adding a generic store facade.
- Write decision before `NodeRemovalStarted` and before participant drain.
  The target preflight probe remains before the decision write.
- Write cleanup-done only after tombstone write succeeds.
- Add `recover_pending_cleanup(source, island, session, remove_id)` or an
  equivalent explicit method.
- Define `MachineRemoveRecovery` result variants next to the recovery method,
  not in the lower-level fact payload module.
- Recovery states:
  - missing decision -> structured missing fact error,
  - decision present but serving commit missing -> pre-commit incomplete status
    with visible nodes and no mutation,
  - decision + serving commit present + no cleanup-done -> recovered pending
    cleanup,
  - cleanup-done present, valid, and backed by the expected tombstone fact ->
    complete command result without RPC,
  - cleanup-done mismatch -> structured fact mismatch error.
- `finish_cleanup` remains the only path that sends stop and tombstone.

**Patterns to follow:**

- `MVP/deploy/src/coordinator.rs::recover_pending_cleanup`
- `MVP/deploy/src/state_machine.rs::recover_pending_cleanup`
- Existing `MachineRemoveCoordinator::finish_cleanup`

**Test scenarios:**

- Probe failure returns before decision/removal-started/serving commit writes.
- Normal execution writes decision after successful probe and before
  removal-started, drain, and serving commit.
- Recovery with decision but missing serving commit returns incomplete without
  stop/tombstone.
- Recovery with decision and serving commit reconstructs `PendingMachineRemove`
  with the original tombstone epoch, visible nodes, reason, and serving commit.
- Recovered pending cleanup still returns `CleanupPending` when projection
  catch-up mismatches.
- Stop failure after recovery returns `CleanupPending` and no tombstone or
  cleanup-done.
- Successful recovered cleanup writes tombstone and cleanup-done in order.
- Recovery after cleanup-done returns complete without participant RPC.
- Recovery with cleanup-done but missing or mismatched tombstone fact returns a
  structured mismatch instead of complete.
- A raw tombstone without cleanup-done does not make recovery complete.

**Verification:** `mvp-machine` tests prove both fresh execution and recovered
execution use the same cleanup gate.

### U3. p2panda Machine Adapter Recovery Writes

**Goal:** Teach `mvp-machine-p2panda` to persist and replay the new
machine-remove command facts through the same p2panda store used for
membership/removal/serving facts.

**Requirements:** Core `mvp-machine` remains p2panda-free; p2panda auth errors
remain structured; replay uses trusted replica import.

**Dependencies:** U1, U2.

**Files:**

- Modify `MVP/machine-p2panda/src/lib.rs`
- Modify `MVP/machine-p2panda/Cargo.toml` only if new dependencies are truly
  needed

**Approach:**

- Extend `PandaMachineFactWriter` to write decision and cleanup-done facts.
- Add explicit writer grants for `/facts/machine-remove/>` in tests and E2E
  fixtures. Existing `/facts/node/*/removal_started/>` and
  `/facts/node/*/tombstoned/>` grants do not cover command facts.
- Keep `PandaMachineFactStore` as the one cloneable store handle for this
  adapter.
- Keep serving writes delegated through `mvp-routing-p2panda`.
- Do not extract a shared p2panda facade in this slice unless implementation
  creates a third exact duplicate with meaningful maintenance cost.

**Patterns to follow:**

- `MVP/machine-p2panda/src/lib.rs`
- `MVP/deploy-p2panda/src/lib.rs`
- `MVP/routing-p2panda/src/lib.rs`

**Test scenarios:**

- Decision and cleanup-done writes return expected keys.
- Duplicate same-payload write is idempotent.
- Conflicting decision or cleanup-done write is a foreground
  `MachineRemoveError::FactConflict`.
- Unauthorized command-fact write returns a structured authorization variant.
- A writer with only node-removal/tombstone grants cannot write command facts.
- Exported operations import into a fresh store through a trusted replica
  session and can be read by the recovery helpers.

**Verification:** `mvp-machine-p2panda` tests cover the adapter before E2E
composition.

### U4. E2E Recovery Proof

**Goal:** Extend `machine-remove-contract` so it proves restart recovery after
serving commit and before stop/tombstone using p2panda operation replay.

**Requirements:** E2E-5 graceful remove, E2E-7 coordinator restart, E2E-9
semantic leverage.

**Dependencies:** U1, U2, U3.

**Files:**

- Modify `MVP/e2e/src/machine_remove_contract.rs`
- Modify `MVP/e2e/src/main.rs` only if a separate scenario name is necessary
  (default: keep the existing scenario)
- Modify `MVP/e2e-proof-plan.md`

**Approach:**

- Run the command until serving commit.
- Drop the original coordinator and do not retain/use its `PendingMachineRemove`
  for cleanup.
- Assert no stop attempts happened before projection catch-up.
- Export p2panda operations, import into a fresh store through a trusted
  replica principal, rebuild projection, and recover pending cleanup.
- Grant the machine writer `/facts/machine-remove/>` explicitly and assert the
  fresh replica import preserves the original author's write authority.
- Register only cleanup participants needed for resumed stop. Track probe,
  drain, and serving-write counts to prove they are not replayed.
- Finish cleanup from recovered pending state.
- Run recovery a second time and assert cleanup-done returns complete without
  RPC.
- Keep the remaining mesh/data-plane proof: non-target traffic works and the
  removed target is rejected after tombstone.

**Patterns to follow:**

- `MVP/e2e/src/deploy_restart_recovery_contract.rs`
- `MVP/e2e/src/machine_remove_contract.rs`
- `MVP/e2e/src/p2panda_projection_fixture.rs`

**Test scenarios:**

- Coordinator is dropped after serving commit before stop/tombstone.
- Recovery reads p2panda-backed facts, not the old in-memory pending value.
- Probe/drain/serving-write counters do not increase during recovery.
- Stop is not attempted before projection catch-up.
- Tombstone happens after recovered stop.
- Cleanup-done makes second recovery complete without RPC.
- Cleanup-done without the expected tombstone fact is rejected in a focused
  recovery assertion.
- Remaining mesh traffic succeeds after coordinator outage/recovery.
- Raw tombstone conflict remains conflict status and does not become command
  completion proof.

**Verification:** `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract`
passes with metrics for recovery read duration,
projection catch-up duration, resumed stop duration, no-replay counts, and
cleanup-done idempotency.

### U5. Documentation, Metrics, And Gate Updates

**Goal:** Record the recovery decision and semantic-leverage evidence so future
maintainers understand why machine remove added command facts instead of a
workflow framework.

**Requirements:** Maintainer docs; semantic leverage; full E2E gate.

**Dependencies:** U1-U4.

**Files:**

- Modify `MVP/overall-plan.md`
- Modify `MVP/architecture.md` only if recovery changes architecture language
- Modify `MVP/primitive-decisions.md`
- Modify `MVP/e2e-proof-plan.md`
- Create `MVP/slice-029-machine-remove-restart-recovery.md`

**Approach:**

- Add a "Changed Since Last Slice" entry for machine-remove decision and
  cleanup-done facts.
- Update E2E-5/E2E-7 current proof status after implementation.
- Include a semantic-leverage ledger:
  - business/domain LOC,
  - adapter/backend LOC,
  - shared foundation LOC,
  - E2E/test LOC,
  - docs LOC,
  - whether new shared substrate was added.
- Note that external workflow crates were scouted but deferred.

**Test scenarios:** Documentation-only unit. Test expectation: none beyond
links/commands matching implemented artifacts.

**Verification:** Slice report names the exact commands run and matches current
LOC after the simplify pass.

## Review Risks

- Accidentally treating tombstone as cleanup completion.
- Replaying pre-commit drain/serving writes during recovery.
- Adding generic command/workflow substrate before the third repeated command
  shape.
- Mixing serving fact ownership back into machine remove.
- Hiding expected p2panda authorization failures in stringly store errors.
- Letting recovery silently mutate from liveness observations instead of
  durable facts plus explicit projection proof.

## Verification Plan

Targeted checks:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-machine -p mvp-machine-p2panda
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-machine -p mvp-machine-p2panda -p mvp-e2e --all-targets -- -D warnings
```

Full gate before push:

```bash
MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
```

Run `ce-simplify-code` after the first green targeted proof, then run
`ce-code-review` with subagents before the implementation is pushed.

## Deferred To Follow-Up

- Automatic recovery on coordinator startup.
- CLI/API retry surface for an operator-facing `machine remove resume`.
- Generic `mvp-commands` / `PhasedCommand`.
- Real runtime/container cleanup backend.
- Kernel WireGuard apply backend.
- p2panda-net cross-process machine-remove recovery beyond local operation
  export/import.
- Active-member or partition-view evidence for recovery decisions.
