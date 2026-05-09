---
title: "feat: Deploy phase hints, ordered execution, and checkpoints"
type: feat
status: active
date: 2026-05-09
origin: docs/plans/2026-05-09-002-feat-deploy-phasing-plan.md
---

# feat: Deploy phase hints, ordered execution, and checkpoints

## Problem Frame

Deploy preview and apply now have a durable default phase, but the model still
executes one opaque unit and commits once at the end. The next larger slice is
to let a deploy manifest describe ordered phases, execute those phases as
separate foreground units, and commit checkpoint phases before later phases run.

This enables the first honest "database before web" primitive: the database
phase can be committed as durable cluster state, while a later web failure is
reported as failure after a committed checkpoint instead of implying a full
rollback.

## Scope

In scope:
- Add manifest intent phase hints under `DeployIntent`.
- Validate phase ids, referenced services/volumes, duplicate membership, and
  unsupported advance policies.
- Derive ordered `DeployPhasePlan` values from manifest hints, defaulting
  unmentioned work into the current single `Deploy` phase.
- Execute phases in plan order instead of assuming exactly one default phase.
- Support `DeployPhaseCommitPolicy::Checkpoint` for a successful phase.
- Commit only the subset of release/volume state owned by a checkpoint phase.
- Preserve current behavior for manifests with no phase hints.
- Add structured deploy state for failure after a checkpoint.

Out of scope:
- `deploy resume`, manual/windowed/wave advancement, and daemon/CLI resume
  surfaces.
- Traffic splitting or gateway percentage routing.
- Automatic rollback.
- External migration command execution.
- Backwards compatibility shims for unreleased preview JSON.

## Requirements

- R1. Existing manifests still produce one immediate phase with
  `EndOfDeploy`/`Reversible`.
- R2. A manifest can declare phases with ordered service/volume membership and
  per-phase commit/rollback policy.
- R3. The planner rejects unknown service or volume references, duplicate phase
  ids, duplicate work ownership, and checkpoint phases without explicit
  non-default rollback policy.
- R4. Multi-phase apply records each phase independently as running, succeeded,
  or failed.
- R5. A checkpoint phase commits its owned release/volume changes before the
  next phase starts.
- R6. If a later phase fails after a checkpoint, earlier checkpoint facts remain
  committed and the deploy record is structurally distinguishable from a
  pre-commit failure.
- R7. Phase-success evidence is best-effort after a checkpoint commit; it must
  not block the committed deploy status.
- R8. The implementation remains command-shaped foreground work; no background
  reconciler advances phases.

## Existing Patterns

- `crates/ployz-types/src/spec.rs` already has `DeployIntent` with service and
  volume hints. Phase hints should live there as planner/operator intent.
- `crates/ployz-types/src/model.rs` already has `DeployPhasePlan`,
  `DeployPhaseRecord`, and small policy enums.
- `crates/ployz-orchestrator/src/deploy/plan.rs` owns `ResolvedPlan::to_preview`
  and the default phase derivation.
- `crates/ployz-orchestrator/src/deploy/execute.rs` owns participant preflight,
  phase record writes, volume move execution, candidate startup, commit status,
  certificates, and cleanup.
- `crates/ployz-orchestrator/src/deploy/lifecycle.rs` owns conversion from
  prepared/startup state into `DeployCommit`.
- `crates/ployz-orchestrator/src/deploy/tests.rs` already covers phase preview,
  phase record lifecycle, volume move execution, post-commit failures, and
  startup phase ordering.

## Key Decisions

1. Put phase hints under `DeployIntent`, not top-level manifest fields.
   These are operation hints for the deploy planner, not desired steady state.

2. Keep advance policy limited to `Immediate` in this slice.
   Manual/windowed/wave policies need a resume command and stored cursor. The
   manifest model may carry the enum shape, but validation rejects unsupported
   non-immediate policies until the operation surface exists.

3. Introduce `Checkpoint` and `ForwardOnly` together.
   A checkpoint means implicit rollback is not guaranteed. For the first slice,
   require checkpoint phases to opt into `ForwardOnly` rollback policy so callers
   cannot accidentally mark irreversible commits as reversible.

4. Commit phase-owned subsets by service and volume ownership.
   A checkpoint phase owns the service and volume work explicitly assigned to
   it. Unmentioned work lands in the final default phase and keeps existing
   end-of-deploy behavior.

5. Use a structured deploy state for failure after checkpoint.
   Callers should branch on `DeployState::FailedAfterCheckpoint` rather than
   parse deploy warnings or phase records to know durable state was committed.

## Implementation Units

### U1. Manifest Phase Hint Schema and Validation

Files:
- `crates/ployz-types/src/spec.rs`
- `crates/ployz-types/src/model.rs`

Approach:
- Extend `DeployIntent` with `phases: Vec<DeployPhaseIntent>`.
- Add phase intent fields: `phase_id`, optional `name`, `order`, `services`,
  `volumes`, `commit_policy`, `rollback_policy`, and `advance_policy`.
- Extend policy enums with `Checkpoint`, `ForwardOnly`, and future-facing
  advance variants only if validation rejects them for now.
- Validate ids are non-empty path-safe tokens, referenced services/volumes
  exist, and no service or volume appears in multiple phase hints.
- Reject `Checkpoint` unless rollback policy is `ForwardOnly`.
- Reject non-`Immediate` advance policy until resume exists.

Test scenarios:
- Manifest with two phases validates and JSON round-trips.
- Duplicate phase ids are rejected.
- Unknown service/volume references are rejected.
- Duplicate work ownership is rejected.
- Checkpoint plus reversible rollback is rejected.
- Manual/windowed/wave advance policy is rejected as unsupported.

### U2. Planner Derives Ordered Phase Plans

Files:
- `crates/ployz-orchestrator/src/deploy/plan.rs`
- `crates/ployz-orchestrator/src/deploy/tests.rs`

Approach:
- Store derived phases in `ResolvedPlan` rather than rebuilding only in
  `to_preview`.
- Use manifest phase hints to partition `DeployPhaseWork`.
- Assign service startup wave numbers from phase order for create/replace
  service work.
- Keep removals and no-op work represented in preview but only start
  create/replace slots.
- Default unmentioned changed services/volumes into the final `Deploy` phase.

Test scenarios:
- `db` checkpoint before `web` preview orders phases correctly.
- Volume move work appears in its assigned phase.
- Unmentioned changed service appears in the default final phase.
- Existing no-hint preview tests still pass.

### U3. Phase-Scoped Commit Planning

Files:
- `crates/ployz-orchestrator/src/deploy/lifecycle.rs`
- `crates/ployz-orchestrator/src/deploy/execute.rs`

Approach:
- Add commit planning helpers that accept a set of phase-owned services and
  volumes.
- For checkpoint phases, include only owned releases, owned volumes, and
  related branch lineage/removed services.
- End-of-deploy commit includes all remaining uncommitted work.
- Track committed services/volumes in the executor to avoid duplicate release
  or volume commits.

Test scenarios:
- DB checkpoint commits the DB release before web startup.
- Final commit does not duplicate already checkpointed DB release.
- Removed services assigned to a checkpoint phase commit removal in that phase.

### U4. Multi-Phase Executor and Failure Semantics

Files:
- `crates/ployz-orchestrator/src/deploy/execute.rs`
- `crates/ployz-orchestrator/src/deploy/tests.rs`
- `crates/ployz-types/src/model.rs`

Approach:
- Replace the default-phase-only executor with an ordered loop over
  `DeployPhasePlan`.
- Execute only work owned by the current phase: volume moves first, then
  startup for services in that phase.
- Write phase records around each phase.
- On checkpoint success, commit phase-owned facts and write committed deploy
  status before recording phase success best-effort.
- On failure before any checkpoint, keep current `Failed` behavior.
- On failure after any checkpoint, write `FailedAfterCheckpoint` with summary
  evidence and leave committed phase facts intact.

Test scenarios:
- Successful two-phase apply records both phases succeeded.
- Web phase failure after DB checkpoint leaves DB release committed and deploy
  state `FailedAfterCheckpoint`.
- Start failure before any checkpoint leaves deploy state `Failed`.
- Phase success evidence failure after checkpoint does not block deploy status.

### U5. Verification and PR Hygiene

Files:
- No new feature files expected beyond the units above.

Approach:
- Run targeted spec, planner, and apply tests first.
- Run `cargo test -p ployz-types`.
- Run `cargo test -p ployz-orchestrator`.
- Run `cargo check --workspace`.
- Run `just test` and `just test-all` before PR because this touches deploy
  execution and public model/schema.
- Run subagent code review and automatically fold all actionable findings back
  into the branch before opening the non-draft PR.

## Risks

- Partial commit planning can accidentally omit unchanged release state needed
  by final cleanup. Mitigation: phase commits include only owned mutations and
  cleanup still derives from the final committed view.
- A checkpoint deploy status can look like a successful complete deploy if state
  naming is vague. Mitigation: add explicit state and tests for later failure.
- Manifest phase hints can imply unsupported resume semantics. Mitigation:
  accept only immediate advancement for now.
- Volume moves and attached services can be split into different phases
  unsafely. Mitigation: planner validation keeps volume move and attached
  service restart in the same phase for this slice.
