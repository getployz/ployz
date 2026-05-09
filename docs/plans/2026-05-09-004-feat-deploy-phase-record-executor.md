---
title: "feat: Add deploy phase records and default phase executor"
type: feat
status: active
date: 2026-05-09
origin: docs/plans/2026-05-09-002-feat-deploy-phasing-plan.md
---

# feat: Add deploy phase records and default phase executor

## Problem Frame

Deploy preview now exposes a typed default `Deploy` phase, but apply still runs
as one opaque block. The next primitive should make the default phase durable
and execute the current work through an explicit phase unit while preserving
today's single final deploy commit semantics.

This gives core and downstream consumers honest phase lifecycle evidence before
adding checkpoint commits, manifest phase hints, pause/resume, or rollout waves.

## Scope Boundaries

In scope:
- Add durable `DeployPhaseRecord` model types.
- Add store APIs for upserting/getting/listing phase records.
- Implement memory and NATS persistence for phase records.
- Refactor deploy apply so current volume move plus service startup work runs
  through one default phase execution unit.
- Write `Running`, `Succeeded`, and `Failed` phase records for the default phase.
- Add phase evidence to apply events/results where useful without changing
  apply semantics.
- Add tests for successful and failed default phase lifecycle recording.

Out of scope:
- Checkpoint commit policy.
- Multi-phase manifests or manifest phase hints.
- Manual/windowed/wave advancement.
- `deploy resume`.
- Phase-aware rollback.
- Partial deploy states such as `FailedAfterCheckpoint`.

## Requirements

- R1. A deploy apply writes a durable default phase record before phase work
  begins.
- R2. A successful deploy apply marks the default phase succeeded before the
  deploy reaches its final committed or cleanup-pending result.
- R3. A failure after phase start marks the default phase failed with structured
  error context.
- R4. Phase records are keyed by namespace, deploy id, and phase id so retries
  or separate deploys cannot reuse stale phase state.
- R5. Existing deploy behavior stays equivalent: one final deploy commit,
  unchanged release/volume commit semantics, unchanged participant preflight
  order.
- R6. Memory and NATS stores support phase record round-trips with key mismatch
  validation where records are decoded from durable keys.
- R7. The executor shape makes phase execution a named unit without introducing
  checkpoint, resume, or rollback behavior ahead of support.

## Existing Patterns

- `crates/ployz-types/src/model.rs` contains `DeployPhasePlan`,
  `DeployPhaseId`, policy enums, `DeployRecord`, and transition-oriented record
  types.
- `crates/ployz-store-api/src/traits.rs` defines `DeployStore`; deploy status is
  already written separately from `commit_deploy`.
- `crates/ployz-store-api/src/memory.rs` stores deploy status records in memory
  under `deploy_records`.
- `crates/ployz-nats/src/store/deploys/mod.rs` stores deploy status records in a
  NATS KV bucket with decode-time key validation.
- `crates/ployz-nats/src/buckets.rs` defines durable authority KV buckets and
  asset metadata.
- `crates/ployz-orchestrator/src/deploy/execute.rs` owns the current apply flow:
  inspect, plan stability validation, write applying status, execute volume
  moves, start candidates, commit deploy, post-commit certificate/cleanup work.
- `crates/ployz-orchestrator/src/deploy/lifecycle.rs` owns `PreparedDeploy`,
  `StartedCandidates`, and commit planning.
- `crates/ployz-orchestrator/src/deploy/tests.rs` already has happy path,
  start failure, volume move failure, retry, and post-commit failure tests.

## Key Decisions

1. Store phase lifecycle records separately from `DeployRecord.summary_json`.
   Summary JSON remains useful preview evidence, but durable phase state should
   be queryable and keyed without parsing deploy summaries.

2. Use an authority durable KV bucket for NATS phase records.
   Phase lifecycle is operator intent/state evidence, not a live observation or
   lease. It should have the same durability posture as deploy status.

3. Start with simple overwrite semantics.
   The executor is the only writer in this slice. A later resume/checkpoint
   slice can add compare-and-set or transition enforcement if needed.

4. Keep one default phase as the only executable phase.
   This refactor creates the phase execution seam; it must not imply support for
   arbitrary manifest phases yet.

5. Mark phase failure only for failures after phase start.
   Reachability, participant inspect, plan-stability, and hostname ownership
   failures happen before the phase is running and should not create misleading
   failed phase records.

## Proposed Model

Add record types equivalent to:

```rust
pub enum DeployPhaseState {
    Running,
    Succeeded,
    Failed,
}

pub struct DeployPhaseFailure {
    pub message: String,
}

pub struct DeployPhaseRecord {
    pub namespace: Namespace,
    pub deploy_id: DeployId,
    pub phase_id: DeployPhaseId,
    pub name: String,
    pub order: u32,
    pub state: DeployPhaseState,
    pub commit_policy: DeployPhaseCommitPolicy,
    pub rollback_policy: DeployPhaseRollbackPolicy,
    pub started_at: u64,
    pub completed_at: Option<u64>,
    pub failure: Option<DeployPhaseFailure>,
}
```

The implementation may add fields if local patterns make them clearly useful,
but it should avoid checkpoint or resume fields until those semantics exist.

## Implementation Units

### U1: Add Phase Record Model and Store Contract

Files:
- `crates/ployz-types/src/model.rs`
- `crates/ployz-store-api/src/traits.rs`

Approach:
- Add `DeployPhaseState`, `DeployPhaseFailure`, and `DeployPhaseRecord`.
- Add `upsert_deploy_phase`, `get_deploy_phase`, and `list_deploy_phases` to
  `DeployStore`.
- Keep list ordering contract explicit: namespace, deploy id, phase order, then
  phase id.

Test scenarios:
- Model serialization round-trips phase states and policies.
- Trait implementers must compile exhaustively with the new methods.

### U2: Persist Phase Records in Memory and NATS Stores

Files:
- `crates/ployz-store-api/src/memory.rs`
- `crates/ployz-nats/src/buckets.rs`
- `crates/ployz-nats/src/store/deploys/mod.rs`

Approach:
- Add memory storage keyed by `(Namespace, DeployId, DeployPhaseId)`.
- Add a NATS phase record bucket, likely `cp_deploy_phases_{authority}`.
- Use a collision-safe key shape that includes namespace, deploy id, and phase
  id.
- Decode records with key/payload validation matching existing deploy status
  patterns.
- Add focused tests next to existing deploy store tests.

Test scenarios:
- Memory store round-trips and lists phase records in contract order.
- NATS decode rejects malformed JSON.
- NATS decode rejects key/payload mismatches.
- Bucket manifest includes the deploy phase bucket as stored intent.

### U3: Introduce Default Phase Execution Unit

Files:
- `crates/ployz-orchestrator/src/deploy/execute.rs`
- `crates/ployz-orchestrator/src/deploy/lifecycle.rs`

Approach:
- Add a small `execute_default_phase` or `execute_phase` helper that:
  - writes a running phase record,
  - executes volume moves,
  - runs service startup,
  - writes succeeded on success,
  - writes failed on execution error.
- Keep the current final deploy commit path intact.
- Keep participant inspect, plan stability, hostname ownership, and applying
  deploy status writes outside the phase execution unit.
- Return started candidate state and events so commit planning remains local to
  existing lifecycle types.

Test scenarios:
- Existing successful deploy apply still commits once and returns committed.
- Existing startup phase ordering remains unchanged.
- Volume move execution ordering remains unchanged.

### U4: Add Apply Lifecycle Tests for Phase Records

Files:
- `crates/ployz-orchestrator/src/deploy/tests.rs`

Approach:
- Extend happy path apply tests to assert a succeeded default phase record.
- Extend start-candidate failure tests to assert a failed default phase record.
- Extend volume-move failure tests to assert a failed default phase record.
- Add a pre-phase failure assertion, if cheap, proving participant inspect
  failure does not write a phase record.
- Add retry/new deploy assertion proving phase records are scoped by deploy id.

Test scenarios:
- Successful apply writes `Running` then final stored state `Succeeded` for
  `DeployPhaseId("deploy")`.
- Start-candidate failure records `Failed` with an error message and no commit
  facts.
- Volume move failure records `Failed` with an error message.
- Participant inspect failure records no phase.
- Retry/new deploy gets its own phase record under its own deploy id.

### U5: Verification and PR Hygiene

Files:
- No additional source files expected.

Approach:
- Run focused store and orchestrator tests first.
- Run `cargo test -p ployz-store-api`.
- Run `cargo test -p ployz-nats`.
- Run `cargo test -p ployz-orchestrator`.
- Run `cargo check --workspace`.
- Run `just test` and `just test-all` before PR because this touches store,
  orchestrator, NATS bucket configuration, and deploy execution.

## Risks

- NATS bucket/key design can accidentally make list-by-deploy inefficient or
  ambiguous. Mitigation: use a stable prefix and add key/payload mismatch tests.
- Writing failed phase records for pre-phase failures would misrepresent what
  actually ran. Mitigation: only create phase records inside the phase executor.
- Refactoring apply can accidentally move commit boundaries. Mitigation: keep
  commit planning unchanged and retain the existing "commit once after starts"
  test.

## Sequencing

1. Add model and trait contract.
2. Implement memory and NATS persistence with tests.
3. Add default phase record builders/executor helper.
4. Wire apply through the helper.
5. Add/extend orchestrator lifecycle tests.
6. Run review and full verification.
