---
title: "feat: Harden deploy-time volume clone branching"
type: feat
status: completed
date: 2026-05-11
origin:
  - VISION.md
  - docs/plans/2026-05-10-003-feat-deploy-volume-snapshot-clone-branching.md
  - docs/plans/2026-05-11-004-feat-branch-prepare-apply-prepared.md
---

# feat: Harden deploy-time volume clone branching

## Summary

Close the deploy-time ZFS volume clone branching primitive by auditing the
implementation that is now on `main`, filling any remaining correctness gaps,
and verifying that schema/API, plan/execute, daemon/ZFS, and real-E2E surfaces
agree. This is a hardening slice, not a new branch command or a second clone
implementation.

## Problem Frame

The core volume clone primitive has landed: branch manifests can request a raw,
crash-consistent same-machine clone; preview exposes clone work; apply executes
participant clone RPCs; and deploy commit records volume branch lineage. The
remaining risk is not missing mechanics, but mismatch between the layers: schema
drift, preview/apply drift holes, cleanup/idempotency edge cases, or E2E
coverage that proves too little.

This slice should make the primitive harder to misuse before more command and
cloud automation is built on top of it.

## Requirements

- R1. Existing deploy-time volume clone branching behavior remains intact.
- R2. Schema/type generation is in parity with the implemented manifest shape.
- R3. Prepared branch clone apply continues to reject source drift before
  participant mutation.
- R4. Clone execution remains foreground work before services that mount the
  cloned volume start.
- R5. Failed clone execution or failed post-clone startup does not commit target
  volume or branch lineage, and cleanup behavior is explicit.
- R6. Reapplying an already-committed branch clone remains idempotent and does
  not reclone or overwrite target data.
- R7. Real ZFS E2E proves snapshot-time copy and post-clone source/target
  divergence.
- R8. Public API/SDK and CLI output continue to expose stable, structured clone
  evidence.

## Scope

In scope:

- Fixes inside the existing volume clone branch implementation.
- Schema/type regeneration if the checked-in package is stale.
- Focused unit/integration tests for plan, execute, store, daemon, and E2E
  behavior where gaps are found.
- Review-driven simplification when it reduces bug risk.

Out of scope:

- Cross-machine volume clone/copy.
- Data masking, quiesce hooks, or cleanup scripts.
- Portal services or attaching PR services to production storage.
- Cloud dashboard changes.
- Replacing the branch compiler or prepared deploy model.

## Existing Patterns

- `crates/ployz-orchestrator/src/deploy/plan.rs` owns source truth resolution,
  clone preview evidence, and baseline inputs.
- `crates/ployz-orchestrator/src/deploy/execute.rs` owns foreground clone work,
  cleanup, and commit evidence construction.
- `crates/ployzd/src/daemon/handlers/volume/zfs.rs` owns node-local ZFS clone
  and cleanup semantics.
- `crates/ployz-store-api/src/deploy_commit_facts.rs` folds committed branch
  lineage into queryable truth.
- `crates/ployz-e2e/src/scenarios/volume_clone_branch_real_smoke.rs` is the
  real ZFS user-flow proof.

## Implementation Units

### U1. Characterize Current Clone Behavior

Files:

- Modify if needed: `crates/ployz-orchestrator/src/deploy/tests.rs`
- Modify if needed: `crates/ployzd/src/daemon/handlers/volume/zfs.rs`
- Modify if needed: `crates/ployz-e2e/src/scenarios/volume_clone_branch_real_smoke.rs`

Approach:

- Run focused clone/branch tests and inspect current failures or weak
  assertions.
- Add characterization tests only where the existing behavior is not locked:
  prepared apply source drift, committed clone idempotency, clone cleanup after
  startup failure, and real ZFS divergence.

Test scenarios:

- Prepared branch clone apply rejects source drift.
- Reapply of committed target clone has no clone work and preserves target data.
- Failed startup after a clone attempts cleanup only for unstarted attached
  services.
- Real E2E validates source and branch diverge after clone.

### U2. Fix Correctness or Idempotency Gaps

Files:

- Modify if needed: `crates/ployz-orchestrator/src/deploy/plan.rs`
- Modify if needed: `crates/ployz-orchestrator/src/deploy/execute.rs`
- Modify if needed: `crates/ployzd/src/daemon/handlers/deploy.rs`
- Modify if needed: `crates/ployzd/src/daemon/handlers/volume/zfs.rs`

Approach:

- Keep policy in the plan/executor layer and ZFS mechanics in daemon/runtime
  code.
- Reject unsupported shapes before participant mutation.
- Preserve committed lineage and target data on idempotent reapply.
- Keep provisional side effects visible and cleanup narrowly scoped.

Test scenarios:

- Clone source or target shape errors fail before participant clone RPC.
- Existing committed clone lineage prevents duplicate clone execution.
- Failed clone or cleanup reports structured deploy errors and does not commit
  lineage.

### U3. Verify Public Schema, API, and CLI Parity

Files:

- Modify if generated drift exists: `packages/deploy/deploy-manifest.schema.json`
- Modify if generated drift exists: `packages/deploy/index.d.ts`
- Modify if needed: `crates/ployz-api/src/deploy.rs`
- Modify if needed: `crates/ployzd/src/cli_io.rs`

Approach:

- Run deploy type generation.
- Keep clone intent and preview evidence names stable.
- Ensure JSON/plain output surfaces clone evidence without requiring callers to
  parse display strings.

Test scenarios:

- Generated schema/types are clean after generation.
- API serde tests cover volume clone fields.
- CLI render tests cover clone preview/evidence when applicable.

### U4. Review and Ship

Files:

- Relevant touched files.

Approach:

- Use subagent review for correctness, reliability, API contract, and test
  coverage.
- Fold all actionable review findings into the PR.
- Open a ready PR and watch CI.

Verification:

- `cargo fmt --check`
- `git diff --check`
- `cargo test -p ployz-types volume_clone`
- `cargo test -p ployz-store-api volume_branch`
- `cargo test -p ployz-orchestrator volume_clone`
- `cargo test -p ployzd volume_zfs`
- `cargo test -p ployz-e2e volume_clone_branch_real_smoke`
- `scripts/generate-deploy-types.sh`
- `just verify-deploy-types`
- `just test-all`

## Risks

- Real ZFS E2E may require Linux/ZFS host support; local unit coverage must
  still be strong when that scenario cannot run locally.
- Cleanup after provisional clone creation must never delete a committed target
  dataset.
- Schema/type generation can produce unrelated churn; inspect generated diffs
  before committing.
