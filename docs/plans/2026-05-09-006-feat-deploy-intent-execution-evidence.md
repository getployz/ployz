---
title: "feat: Persist deploy intent and execution evidence"
type: feat
status: active
date: 2026-05-09
origin: docs/plans/2026-05-08-004-feat-service-branching-deploy-plan.md
---

# feat: Persist deploy intent and execution evidence

## Summary

Persist typed, queryable deploy evidence that explains why branch and movement
facts exist after a deploy. The current stack can plan branch sources, execute
ZFS volume moves, and commit checkpointed phases, but volume movement proof and
phase commit linkage are still transient or implicit. This slice records the
operator intent, phase/work association, checkpoint commit linkage, and verified
volume transfer outcome at the same durable boundaries as releases and volume
ownership.

## Problem Frame

Ployz is moving from ordinary deploys toward explicit branch, migrate, promote,
and rollback primitives. The current deploy model now has strong execution
pieces: intent hints, previews, phase records, checkpoint commits, and blocking
ZFS volume movement. The remaining gap is causal evidence. An operator can see
that a volume now lives on another machine, but durable state does not yet answer
which deploy moved it, from where, with which transfer snapshot, or which phase
and checkpoint committed that fact.

Branch lineage already has a committed fact record, so this plan extends the
same idea to movement evidence and phase commit linkage rather than inventing a
separate workflow. Evidence is durable deploy truth when it explains committed
facts. Live observations, transfer progress, and raw logs stay outside this
surface.

## Requirements

- R1. Persist typed volume movement evidence for every committed
  `VolumeChange::Move`.
- R2. Movement evidence records include namespace, volume, source machine,
  target machine, deploy id, phase id, transfer snapshot name, snapshot GUID,
  transferred bytes, final volume owner, and creation timestamp.
- R3. Movement evidence commits atomically with the volume ownership commit that
  makes the move durable.
- R4. A failed transfer or failed deploy commit must not expose successful
  movement evidence as committed truth.
- R5. Checkpoint phase commits carry typed linkage from phase id to the deploy
  commit id that wrote phase-owned facts.
- R6. Final deploy commits carry equivalent linkage for end-of-deploy phase
  success where useful, without requiring consumers to parse synthetic deploy ids.
- R7. Store implementations expose list/query methods for movement evidence and
  preserve deterministic ordering.
- R8. NATS and memory stores produce equivalent evidence snapshots and reject or
  ignore malformed/key-mismatched evidence the same way existing commit facts do.
- R9. Existing routing projections remain unchanged. Gateway and DNS continue
  to consume ordinary release/instance/volume facts, not intent evidence.
- R10. Do not persist full raw manifests in this slice because service specs may
  contain sensitive environment values.

## Assumptions

- The correct next large slice is durable evidence, not a new command surface.
  Commands such as `migrate` and `branch` can render deploy manifests once the
  core facts are queryable.
- Movement evidence should live with deploy commit facts rather than phase
  records alone. Phase records describe execution state; movement evidence
  explains committed storage truth.
- Branch intent evidence before commit remains out of scope unless it can be
  represented without storing raw specs. Branch lineage already records the
  committed release relationship.

## Scope Boundaries

In scope:
- Durable movement evidence records in `ployz-types`.
- Store API, memory store, and NATS store support for movement evidence.
- Executor plumbing that carries ZFS transfer results into phase/final commits.
- Phase record linkage to checkpoint/final commit ids.
- Tests covering commit atomicity, checkpoint partitioning, ordering, and store
  parity.
- Documentation updates for deploy truth and evidence.

Out of scope:
- Raw manifest persistence or manifest redaction.
- New CLI/API commands for listing evidence.
- Portal services, snapshot-clone branch volumes, and promotion traffic
  switching.
- Source dataset cleanup after a move.
- Background retry/reconciliation of evidence writes.

## Existing Patterns

- `crates/ployz-types/src/model.rs` owns durable deploy-facing models such as
  `ServiceBranchLineageRecord`, `VolumeRecord`, `DeployPhaseRecord`, and preview
  evidence structs.
- `crates/ployz-store-api/src/traits.rs` defines `DeployCommit` as the atomic
  durable deploy fact boundary.
- `crates/ployz-store-api/src/deploy_commit_facts.rs` folds committed deploy
  facts into queryable snapshots and already handles service branch lineage.
- `crates/ployz-store-api/src/memory.rs` and
  `crates/ployz-nats/src/store/deploys/mod.rs` are the two deploy store
  implementations that must stay behaviorally equivalent.
- `crates/ployz-orchestrator/src/deploy/execute.rs` already receives
  `MoveVolumeResult` from blocking ZFS movement and builds phase-scoped commits.
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`
  requires durable truth and live observation to stay separate.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`
  reinforces validating final participants and persisted intent before mutation.

## Key Technical Decisions

1. Store movement evidence as committed deploy facts.
   A successful transfer is not durable ownership truth until `commit_deploy`
   writes the moved `VolumeRecord`. The evidence should be committed in the same
   command so consumers never observe proof for a move that did not become true.

2. Keep phase records focused on execution state, but add commit linkage.
   `DeployPhaseRecord` should not become a bag of transfer payloads. It should
   record which commit id made a phase durable so operators can join phase state
   to committed facts without parsing synthetic deploy ids.

3. Carry transfer results through the executor explicitly.
   `DeployApplyResult.events` are user-facing progress strings, not durable
   input. The executor needs structured in-memory results keyed by volume until
   the phase/final commit is built.

4. Keep routing projections unchanged.
   Lineage and movement evidence answer "why" for operators and automation.
   Runtime routing still follows committed releases, volumes, and instances.

5. Avoid raw manifest storage.
   The deploy manifest may contain sensitive values. This slice persists narrow
   canonical evidence only.

## Implementation Units

### U1. Add Volume Movement Evidence Model

**Goal:** Define the durable record shape for committed volume movement proof.

**Files:**
- Modify: `crates/ployz-types/src/model.rs`
- Test: `crates/ployz-types/src/model.rs`

**Approach:**
- Add a `VolumeMovementRecord` or similarly named model near
  `ServiceBranchLineageRecord`.
- Include namespace, volume name, from/to machines, deploy id, optional phase id,
  snapshot name, snapshot GUID, bytes transferred, final owner machine, and
  created timestamp.
- Derive the same serialization/schema traits used by other public model
  records.

**Test Scenarios:**
- Record JSON round-trips with all required fields.
- Optional phase id serializes absent for non-phased or legacy-style records if
  the chosen shape permits absence.

### U2. Extend Deploy Commit Facts and Store Queries

**Goal:** Make movement evidence part of the atomic deploy commit surface.

**Files:**
- Modify: `crates/ployz-store-api/src/traits.rs`
- Modify: `crates/ployz-store-api/src/deploy_commit_facts.rs`
- Modify: `crates/ployz-store-api/src/driver.rs`
- Modify: `crates/ployz-store-api/src/memory.rs`
- Test: `crates/ployz-store-api/src/deploy_commit_facts.rs`
- Test: `crates/ployz-store-api/src/memory.rs`

**Approach:**
- Add `volume_movements` to `DeployCommit`.
- Fold movement evidence into `DeployCommitFacts` under a stable key.
- Add `list_volume_movements(namespace)` and, if useful for tests and future
  commands, `list_volume_movements_for_volume(namespace, volume)`.
- Remove movement evidence when the corresponding volume is removed from the
  namespace.

**Test Scenarios:**
- Commit records movement evidence and returns it in deterministic identity
  order.
- Recommitting the same commit is idempotent.
- Removing a volume removes its movement evidence without touching other
  volumes or namespaces.
- Failed commit injection in memory store does not expose new movement evidence.

### U3. Persist Movement Evidence in NATS Store

**Goal:** Keep the replicated NATS store behavior equivalent to memory store.

**Files:**
- Modify: `crates/ployz-nats/src/store/deploys/mod.rs`
- Test: `crates/ployz-nats/src/store/deploys/mod.rs`

**Approach:**
- Reuse the existing deploy commit log/facts fold if possible instead of adding
  a separate bucket.
- Ensure replayed commit facts reconstruct movement evidence exactly like memory
  store.
- Extend key/payload mismatch and malformed commit tests as needed.

**Test Scenarios:**
- NATS commit replay returns movement evidence after restart/reload.
- Duplicate commit publish does not duplicate evidence.
- Volume removal removes movement evidence from the reconstructed facts.

### U4. Carry ZFS Move Results Into Phase Commits

**Goal:** Convert blocking move execution results into committed movement
evidence at the correct phase boundary.

**Files:**
- Modify: `crates/ployz-orchestrator/src/deploy/execute.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/lifecycle.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/participant.rs` if transfer id
  is added to the participant result.
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**
- Change phase execution output to return structured volume move results keyed
  by volume.
- Accumulate movement results across phases.
- When building a checkpoint commit, include only movement evidence for moved
  volumes owned by that checkpoint phase.
- When building the final commit, include only remaining movement evidence.
- Ensure evidence uses the same commit timestamp and deploy id boundary as the
  corresponding volume ownership commit.

**Test Scenarios:**
- Happy path: moved volume commits `VolumeRecord.machine_id` and movement
  evidence with snapshot GUID/bytes in the same commit.
- Error path: move transfer success followed by commit failure does not expose
  movement evidence.
- Checkpoint path: movement in a checkpoint phase commits evidence before later
  phases run.
- Final path: movement in an end-of-deploy phase commits evidence only at the
  final commit.
- Failure after checkpoint leaves checkpoint movement evidence visible and
  later uncommitted movement evidence absent.

### U5. Add Phase Commit Linkage

**Goal:** Make phase records explicitly identify the deploy commit that made
phase-owned facts durable.

**Files:**
- Modify: `crates/ployz-types/src/model.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/execute.rs`
- Modify: `crates/ployz-store-api/src/memory.rs`
- Modify: `crates/ployz-nats/src/store/deploys/mod.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`
- Test: `crates/ployz-store-api/src/memory.rs`
- Test: `crates/ployz-nats/src/store/deploys/mod.rs`

**Approach:**
- Add an optional `commit_deploy_id` to `DeployPhaseRecord`.
- Set it when a checkpoint phase commit succeeds and when an end-of-deploy
  phase is marked succeeded after the final commit.
- Preserve `NoStoreCommit` phases without commit linkage.

**Test Scenarios:**
- Checkpoint phase record has the synthetic checkpoint commit id after success.
- End-of-deploy phase record has the final deploy id after final commit.
- Failed phases and no-store phases do not claim commit linkage.
- Lock-loss terminalization preserves absence of commit linkage for failed
  in-flight phases.

### U6. Documentation and Schema Parity

**Goal:** Keep downstream readers aligned with the new durable evidence model.

**Files:**
- Modify: `docs/routing-and-deploys.md`
- Modify generated package files only if schema generation changes due public
  model exports.

**Approach:**
- Update deploy truth documentation to distinguish preview evidence, phase
  records, committed lineage/movement evidence, and live transfer observations.
- If public generated schemas change, regenerate them through the existing
  script rather than hand-editing generated output.

**Test Scenarios:**
- Documentation names the commit boundary and the non-routing nature of
  evidence.
- Generated files are updated only when the Rust schema changed.

## Verification Plan

- `cargo test -p ployz-types`
- `cargo test -p ployz-store-api`
- `cargo test -p ployz-nats`
- `cargo test -p ployz-orchestrator`
- `cargo check --workspace`
- `just test`
- `just test-all`

## Review Risks

- Writing evidence after side effects but before commit can expose false truth.
  Evidence must be part of `DeployCommit`, not a separate success log.
- Phase commit linkage can become ambiguous if consumers parse deploy id strings.
  Store a typed `commit_deploy_id` instead.
- Movement evidence must be partitioned by phase, otherwise checkpoint commits
  can accidentally claim later moves.
- Adding store trait methods touches every fake and backend; tests should catch
  default or empty implementations that silently hide evidence.
