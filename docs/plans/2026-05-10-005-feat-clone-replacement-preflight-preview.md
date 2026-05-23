---
title: Expose Clone Replacement Preflight in Deploy Evidence
date: 2026-05-10
status: active
origin: lfg-full-slice
---

# Expose Clone Replacement Preflight in Deploy Evidence

## Problem

The previous clone replacement slice centralized the safety behavior for cloned
volumes: before a deploy replaces clone-backed volume datasets, the coordinator
drains and removes uncommitted namespace instances that are not represented by
committed release slots.

That makes execution safer, but the behavior is still mostly hidden. A deploy
preview can show that a volume will be cloned, yet it does not show that the
clone batch has a replacement preflight that may stop uncommitted instances.
Likewise, apply events currently show the individual drain/remove actions only
when matching runtime instances exist. If no candidates are found, there is no
operator-facing evidence that the preflight ran.

This should be explicit. Deploys are operational primitives; their
preconditions, safety work, and effects should be visible to CLI, SDK, cloud,
and agent consumers before and during mutation.

## Scope

In scope:

- Add a typed deploy preview model for clone replacement preflight work.
- Include clone replacement preflight entries in `DeployPreview` for phases
  that create clone-backed volumes.
- Emit an apply event before clone RPCs that records the preflight batch even
  when no uncommitted instances need to be stopped.
- Add regression coverage for preview evidence and apply event ordering.

Out of scope:

- Exact runtime candidate enumeration in preview. Preview is plan-derived and
  does not inspect live participants.
- Durable clone artifact records.
- Moving backend stale-target replacement authority out of ZFS.
- General operation transaction or rollback framework.
- Changing volume move behavior.

## Existing Patterns

- `crates/ployz-types/src/model.rs` is the public model surface for
  `DeployPreview`, volume clone plans, volume move plans, and deploy events.
- `crates/ployz-orchestrator/src/deploy/plan.rs` derives preview data from
  `ResolvedPlan` without performing participant RPCs.
- `crates/ployz-orchestrator/src/deploy/execute.rs` owns mutation ordering and
  already emits coarse deploy events before and after mutation RPCs.
- `docs/plans/2026-05-10-004-fix-deploy-clone-replacement-preflight.md`
  established the execution-side ordering: stop uncommitted namespace instances
  before clone RPCs that may replace datasets.

## Key Decisions

- Model preflight as phase-scoped deploy preview evidence, not as a hidden
  warning string. Warnings are for degraded or surprising conditions; this is
  planned work.
- Use enum fields for preflight action and target scope so clients do not need
  to parse prose.
- Keep preview candidate scope conservative: the plan can state that the
  coordinator will check uncommitted namespace instances before cloning, while
  apply events reveal the concrete stopped instances after participant
  inspection.
- Preserve the existing clone execution guard. This slice exposes and tests the
  contract; it does not relax the safety behavior.

## Implementation Units

### Unit 1: Preview Model

Files:

- `crates/ployz-types/src/model.rs`
- `crates/ployzd/src/daemon/handlers/deploy.rs`

Change:

- Add a `VolumeClonePreflightPlan` model with:
  - `phase_id`,
  - cloned `volumes`,
  - an enum action for drain/remove-before-clone-replacement,
  - an enum scope for uncommitted namespace instances.
- Add `volume_clone_preflights` to `DeployPreview` with serde defaults and
  empty-list elision.
- Update daemon test fixtures that construct serialized `DeployPreview`
  values.

Rationale:

- The preview schema becomes the stable API surface for downstream consumers
  that need to explain deploy effects before mutation.
- Enum fields keep the contract machine-readable without string parsing.

Test scenarios:

- Existing deploy handler preview round-trip tests continue to deserialize
  stored preview JSON.
- Empty preflight lists remain omitted from serialized preview JSON.

### Unit 2: Preview Derivation

Files:

- `crates/ployz-orchestrator/src/deploy/plan.rs`
- `crates/ployz-orchestrator/src/deploy/tests.rs`

Change:

- Derive one preflight preview entry per deploy phase that contains clone-backed
  volume creation work.
- Include the phase id and the clone volume names in phase work order.
- Add a preview test proving clone-backed volume deploys expose both
  `volume_clones` and `volume_clone_preflights`.

Rationale:

- Clone replacement safety is phase-scoped: deploy execution batches clone work
  per phase, so preview evidence should match that boundary.

Test scenarios:

- A single clone-backed volume in one phase creates one preflight preview entry.
- A deploy without clone-backed volume creation produces no preflight entry.

### Unit 3: Apply Evidence

Files:

- `crates/ployz-orchestrator/src/deploy/execute.rs`
- `crates/ployz-orchestrator/src/deploy/tests.rs`

Change:

- Emit a `preflight_clone_replacement` event before draining uncommitted
  instances and before the first `clone_volume` RPC in a clone batch.
- Include the clone volumes in the event message.
- Add apply regression coverage for event ordering.

Rationale:

- Operators need evidence that the preflight ran even when there were no stale
  candidates to stop.
- Ordering assertions make the safety contract hard to regress.

Test scenarios:

- The preflight event appears before `stop_uncommitted_instance` and
  `clone_volume` events when stale uncommitted candidates exist.
- The preflight event still appears before `clone_volume` when no candidates are
  stopped.

## Risks

- The preview cannot promise exact candidates because it is intentionally
  resolved without live participant inspection. The model should describe the
  planned preflight scope, not pretend to know live runtime state.
- Public model changes may require fixture updates in daemon tests. There is no
  compatibility shim planned because this project is still greenfield.

## Verification

- `cargo fmt --check`
- `cargo test -p ployz-orchestrator volume_clone`
- `cargo test -p ployz-types deploy_preview`
- `cargo test -p ployzd deploy`
- `git diff --check -- crates/ployz-types/src/model.rs crates/ployz-orchestrator/src/deploy/plan.rs crates/ployz-orchestrator/src/deploy/execute.rs crates/ployz-orchestrator/src/deploy/tests.rs crates/ployzd/src/daemon/handlers/deploy.rs docs/plans/2026-05-10-005-feat-clone-replacement-preflight-preview.md`
