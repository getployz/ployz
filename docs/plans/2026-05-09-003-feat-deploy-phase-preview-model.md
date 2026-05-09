---
title: "feat: Add deploy phase preview model"
type: feat
status: active
date: 2026-05-09
origin: docs/plans/2026-05-09-002-feat-deploy-phasing-plan.md
---

# feat: Add deploy phase preview model

## Problem Frame

Deploy planning already knows about ordered startup waves and planned volume
moves, but preview output still exposes those as separate lists. The next core
primitive slice should make phase boundaries explicit in the domain model before
changing execution, storage, resume, or checkpoint behavior.

This keeps the first phase slice low-risk: existing deploy apply behavior stays
the same, while preview consumers can begin seeing a stable ordered phase plan
with policy metadata.

## Scope Boundaries

In scope:
- Add typed deploy phase preview structures to `crates/ployz-types/src/model.rs`.
- Add `DeployPreview.phases` as additive JSON evidence.
- Derive a default phase from the current resolved plan.
- Represent volume moves as phase work before service startup work.
- Preserve existing `DeployPreview.services`, `service_branch_sources`, and
  `volume_moves` fields.
- Add focused orchestrator tests for default phase evidence.

Out of scope:
- Manifest phase hints.
- Phase records in the store.
- Checkpoint commits.
- Resume or pause commands.
- Executor refactors beyond naming/internal preview helpers required for this
  model.
- Backwards compatibility gymnastics for unreleased consumers.

## Requirements

- R1. A basic manifest produces exactly one default phase in preview output.
- R2. The default phase includes stable id, name, order, participants, work
  items, commit policy, rollback policy, and advance policy.
- R3. Volume moves appear as explicit phase work and precede service startup
  work in the same default phase.
- R4. Existing preview fields remain populated so current CLI/API assertions
  continue to pass.
- R5. The phase model is typed and serializable; callers should not parse
  warnings or free-form strings to understand phase boundaries.
- R6. The implementation stays additive and does not alter deploy apply
  behavior in this slice.

## Existing Patterns

- `crates/ployz-types/src/model.rs` defines serializable deploy preview types:
  `ServicePlan`, `VolumeMovePlan`, and `DeployPreview`.
- `crates/ployz-orchestrator/src/deploy/plan.rs` owns `ResolvedPlan::to_preview`
  and already has all service, participant, branch, and volume move evidence.
- `PlannedService.phase` currently drives startup order only. It should be
  treated as startup-wave detail, not the public phase model.
- `crates/ployz-orchestrator/src/deploy/tests.rs` already covers volume move
  preview evidence, branch source preview evidence, and startup phase ordering.
- `crates/ployzd/src/daemon/handlers/deploy.rs` has literal `DeployPreview`
  construction in tests that will need the additive `phases` field.

## Key Decisions

1. Add phase evidence to `DeployPreview`, not `DeployManifest`.
   Manifest hints come after the executor and storage semantics can honor them.
   This slice only exposes what the planner already knows.

2. Use one default phase.
   Current behavior is one deploy transaction. A single phase named `Deploy`
   accurately describes the existing apply path while giving future slices a
   place to add checkpoints and manual advancement.

3. Model phase work as typed enum variants.
   Volume moves, service starts, service removals, and no-store evidence should
   be branchable data. This avoids free-form event strings becoming the public
   contract.

4. Keep policy enums small.
   Start with `EndOfDeploy`, `Reversible`, and `Immediate`, plus enough work
   variants to describe current plans. Additional checkpoint, forward-only, and
   manual policies should arrive with execution support.

## Proposed Shape

The exact Rust names may vary, but the model should be equivalent to:

```rust
pub struct DeployPhasePlan {
    pub phase_id: String,
    pub name: String,
    pub order: u32,
    pub participants: Vec<MachineId>,
    pub work: Vec<DeployPhaseWork>,
    pub commit_policy: DeployPhaseCommitPolicy,
    pub rollback_policy: DeployPhaseRollbackPolicy,
    pub advance_policy: DeployPhaseAdvancePolicy,
}

pub enum DeployPhaseWork {
    Service { service: String, action: DeployChangeKind },
    VolumeMove { volume: String, from_machine: MachineId, to_machine: MachineId },
}
```

The default phase should use:
- `phase_id = "deploy"`
- `name = "Deploy"`
- `order = 0`
- `commit_policy = EndOfDeploy`
- `rollback_policy = Reversible`
- `advance_policy = Immediate`

## Implementation Units

### U1: Add Typed Phase Preview Model

Files:
- `crates/ployz-types/src/model.rs`

Approach:
- Add serializable phase policy enums and work enum.
- Add `DeployPhasePlan`.
- Add `phases: Vec<DeployPhasePlan>` to `DeployPreview`.
- Use serde defaults if needed by local tests, but do not design around
  long-lived backwards compatibility.

Test scenarios:
- Existing model serialization tests, if any, continue to compile.
- Exhaustive Rust construction catches all literal `DeployPreview` builders.

### U2: Derive Default Phase from Resolved Plan

Files:
- `crates/ployz-orchestrator/src/deploy/plan.rs`
- `crates/ployz-orchestrator/src/deploy/tests.rs`

Approach:
- Add a helper on `ResolvedPlan` that builds the default phase.
- Include all plan participants in the phase participants list.
- Add volume move work first, preserving existing move ordering.
- Add service work for services whose action is not `Unchanged`.
- Keep existing `volume_moves` and `services` preview output untouched.

Test scenarios:
- Basic manifest preview has one `Deploy` phase with `EndOfDeploy`,
  `Reversible`, and `Immediate` policies.
- Volume move preview contains `VolumeMove` work before the affected service
  work.
- Branch source preview still surfaces branch lineage while the phase lists the
  service create work.

### U3: Update Preview Construction Call Sites

Files:
- `crates/ployzd/src/daemon/handlers/deploy.rs`
- Any other compile-reported literal `DeployPreview` construction.

Approach:
- Update test fixtures or literal preview payloads to include `phases`.
- Prefer empty phases only for tests that intentionally do not exercise deploy
  planning; planned previews should use the real helper path.

Test scenarios:
- `cargo test -p ployz-orchestrator deploy::tests::resolve_plan_volume_move_places_attached_service_on_target`
- Relevant `ployzd` deploy handler tests still compile and pass.

### U4: Verification and PR Hygiene

Files:
- No additional source files expected.

Approach:
- Run focused orchestrator tests for phase preview, branch source preview, and
  volume move preview.
- Run `cargo test -p ployz-orchestrator`.
- Run `cargo check --workspace`.
- Review diffs for accidental execution semantics changes.

## Assumptions

- This branch may be stacked on the ZFS volume move execution branch until that
  PR lands, because useful phase work includes volume move evidence introduced
  there.
- Current consumers are greenfield enough that we do not need compatibility
  shims for the new `DeployPreview.phases` field.
