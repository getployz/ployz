---
title: "feat: Deploy-time volume snapshot clone branching"
type: feat
status: active
date: 2026-05-10
origin:
  - VISION.md
  - docs/plans/2026-05-08-004-feat-service-branching-deploy-plan.md
  - docs/plans/2026-05-09-001-feat-zfs-volume-move-execution-plan.md
  - docs/plans/2026-05-09-006-feat-deploy-intent-execution-evidence.md
---

# feat: Deploy-time volume snapshot clone branching

## Summary

Add the first deploy primitive for stateful environment branching: a target
namespace can declare a new managed volume whose initial dataset is a local ZFS
snapshot clone of a committed source namespace volume. Preview resolves source
truth and clone placement, apply executes a foreground snapshot/clone before
candidate startup, and deploy commit records the target `VolumeRecord` plus
durable clone lineage/evidence.

This is intentionally not full `ployzctl branch` yet. It is the core deploy
primitive that later branch commands and cloud PR environments can render.

## Problem Frame

Ployz can now move existing single-scope volumes during deploy apply and commit
movement evidence. Branch environments need a different state operation: create
a new target identity from a pinned source snapshot while leaving the source
volume owner and source dataset unchanged.

Modeling clone as a move would be wrong. A move preserves identity and changes
ownership. A branch clone creates a new `namespace/volume` identity with source
lineage. That distinction matters for rollback, promotion, cleanup, and future
cloud automation.

## Requirements

- R1. A deploy manifest can declare a target volume initialized from a committed
  source namespace volume.
- R2. The v1 clone is explicitly same-machine and ZFS-backed. Cross-machine
  branch copy is rejected until modeled as send/receive copy with separate
  evidence semantics.
- R3. Clone intent is explicit. The manifest must declare raw data copy and
  crash-consistent snapshot behavior so PR branches cannot silently inherit
  production data.
- R4. Preview shows the source namespace/volume, source machine, target machine,
  snapshot name, target volume, and attached target services.
- R5. Apply re-resolves source truth and rejects stale source owner, missing
  source, existing committed target volume, unavailable source machine, or
  unsupported target/source shape before candidate startup or commit.
- R6. Apply executes snapshot/clone as foreground deploy work before starting
  services that mount the target volume.
- R7. A successful clone does not become durable truth until the deploy commit
  writes the target `VolumeRecord` and clone lineage evidence together.
- R8. Clone lineage/evidence is queryable through the same deploy commit fact
  machinery as service branch lineage and volume movement evidence.
- R9. Failed snapshot/clone/verification fails the deploy visibly and does not
  commit a target volume or clone evidence.
- R10. Existing fresh deploy and volume move behavior remains unchanged.
- R11. Public schema and generated TypeScript deploy package stay in parity.
- R12. Real ZFS E2E proves the clone contains source data at snapshot time and
  source/target writes diverge after the branch.

## Scope Boundaries

In scope:

- `VolumeIntent::Clone` or equivalent manifest vocabulary on volume hints.
- Same-machine local ZFS snapshot clone from source dataset to target dataset.
- Explicit v1 policies: raw data copy and crash-consistent snapshot.
- Preview, plan fingerprint, participant execution, phase work, commit evidence,
  memory/NATS store folding, generated schema/types, and real ZFS E2E.

Out of scope:

- Full `ployzctl branch` command.
- Cross-machine clone/copy via send/receive.
- Application quiesce hooks, anonymization commands, or cleanup scripts.
- ZFS promote, source snapshot deletion, dependency cleanup, or branch
  promotion semantics.
- Portal services or attaching a PR service directly to production storage.
- Raw manifest persistence.

## Key Technical Decisions

1. Clone is a new volume intent, not a move.
   The target `VolumeDeclaration` provides the new identity. The intent only
   explains source lineage and clone policy.

2. v1 clones are source-machine-local.
   ZFS `clone` is copy-on-write within the same pool. The target `VolumeRecord`
   should be placed on the source machine. Cross-machine branching is a later
   transfer/copy primitive.

3. Data inheritance must be explicit.
   The manifest should include `data_policy: raw` and
   `consistency: crash_consistent` in v1. Unsupported `empty`, `anonymized`,
   `quiesced`, or `source_stopped` policies are rejected until implemented.

4. Clone evidence commits with target volume ownership.
   A snapshot and target dataset may exist as side effects before commit, but
   they are not durable deploy truth. `DeployCommit` must contain both the
   target `VolumeRecord` and matching clone lineage record.

5. Orchestrator stays storage-backend agnostic.
   The deploy executor calls a participant `clone_volume` method and receives
   verified evidence. ZFS command details stay in `ployzd` and
   `ployz-runtime-backends`.

6. Provisional side effects are visible but not silently trusted.
   If the target dataset already exists before commit, retry may adopt it only
   when verification proves it is the exact clone of the expected source
   snapshot. Mismatched target datasets are rejected.

## Existing Patterns

- `crates/ployz-types/src/spec.rs` already contains deploy intent hints and
  volume move intent validation.
- `crates/ployz-orchestrator/src/deploy/plan.rs` resolves manifest volumes,
  service-volume pins, branch source lineage, volume moves, phases, and preview.
- `crates/ployz-orchestrator/src/deploy/execute.rs` executes volume moves before
  startup and carries structured transfer results into phase/final commits.
- `crates/ployz-store-api/src/deploy_commit_facts.rs` folds branch lineage and
  volume movement evidence into queryable commit facts.
- `crates/ployzd/src/daemon/handlers/volume/zfs.rs` owns daemon-level ZFS
  operation plumbing.
- `crates/ployz-runtime-backends/src/storage/zfs.rs` owns command-level ZFS
  helpers such as dataset ensure, snapshot, send, recv, and destroy.
- `crates/ployz-e2e/src/scenarios/migrate_service_real_smoke.rs` and
  `crates/ployz-e2e/src/scenarios/drain_aware_redeploy_real_smoke.rs` are the
  real-ZFS templates for this scenario.

## Implementation Units

### U1. Add Manifest, Preview, and Durable Model Shapes

**Goal:** Define the public and durable vocabulary without implementing ZFS
execution yet.

**Files:**

- Modify: `crates/ployz-types/src/spec.rs`
- Modify: `crates/ployz-types/src/model.rs`
- Modify: `crates/ployz-types/src/error.rs`
- Test: `crates/ployz-types/src/spec.rs`
- Generated later: `packages/deploy/deploy-manifest.schema.json`
- Generated later: `packages/deploy/index.d.ts`

**Approach:**

- Add a volume clone intent with source namespace/volume plus v1 policy fields:
  `data_policy: raw` and `consistency: crash_consistent`.
- Add preview/evidence models such as `VolumeClonePlan` and
  `VolumeBranchLineageRecord`.
- Add `DeployPhaseWork::VolumeClone` so phases can expose clone work like
  volume move work.
- Reject unsupported policy values in validation, while still keeping the enum
  exhaustive for future explicit support.

**Test Scenarios:**

- Manifest with no clone intent validates unchanged.
- Clone intent validates with non-empty source namespace/volume and explicit
  supported policies.
- Clone intent rejects empty source, unsupported policy, and invalid namespace.
- Generated schema/types include the implemented clone shape.

### U2. Resolve Clone Plans and Fingerprint Source Truth

**Goal:** Preview clone work and make source truth part of plan stability.

**Files:**

- Modify: `crates/ployz-orchestrator/src/deploy/plan.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**

- Resolve source `VolumeRecord` from `source_namespace/source_volume`.
- Reject same target/source identity, missing source volume, existing committed
  target volume, shared source/target scope, non-ZFS mode, unavailable source
  machine, or non-storage-capable source machine.
- Pin target volume and attached target services to the source machine for v1.
- Include source volume ownership and modification metadata in the plan
  fingerprint so preview/apply rejects stale source truth.
- Add clone work to phase planning and preview output.

**Test Scenarios:**

- Happy path: `pr-39/data` plans as a clone of `prod/data` on the source
  machine, and attached service slots pin to that machine.
- Error path: source missing, target exists, target equals source, source is
  shared, target is shared, source machine missing/unavailable, or source mode
  is not ZFS.
- Stability: changing source owner between initial and final plan rejects apply
  before clone execution.

### U3. Commit Clone Lineage Evidence

**Goal:** Make clone evidence a durable deploy commit fact.

**Files:**

- Modify: `crates/ployz-store-api/src/traits.rs`
- Modify: `crates/ployz-store-api/src/deploy_commit_facts.rs`
- Modify: `crates/ployz-store-api/src/driver.rs`
- Modify: `crates/ployz-store-api/src/memory.rs`
- Modify: `crates/ployz-nats/src/store/deploys/mod.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/execute.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/lifecycle.rs`
- Test: `crates/ployz-store-api/src/deploy_commit_facts.rs`
- Test: `crates/ployz-store-api/src/memory.rs`
- Test: `crates/ployz-nats/src/store/deploys/mod.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**

- Extend `DeployCommit` with `volume_branches` or `volume_clones`.
- Fold evidence only when it matches a committed target volume in the same
  namespace and commit.
- Remove target clone evidence when the target volume is removed.
- Carry executed clone results into checkpoint/final commit construction,
  including phase id and commit deploy id.

**Test Scenarios:**

- Evidence commits atomically with target `VolumeRecord`.
- Evidence is ignored/rejected by commit facts when target volume is absent or
  final machine does not match.
- Target volume removal removes target clone lineage.
- Failed commit does not expose clone evidence.

### U4. Add Participant and ZFS Clone Execution

**Goal:** Execute and verify same-machine ZFS snapshot clone before startup.

**Files:**

- Modify: `crates/ployz-orchestrator/src/deploy/participant.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/execute.rs`
- Modify: `crates/ployz-api/src/request.rs`
- Modify: `crates/ployz-api/src/response.rs`
- Modify: `crates/ployz-api/src/volume.rs`
- Modify: `crates/ployzd/src/daemon/handlers/deploy.rs`
- Modify: `crates/ployzd/src/daemon/handlers/volume/zfs.rs`
- Modify: `crates/ployz-runtime-backends/src/storage/zfs.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`
- Test: `crates/ployzd/src/daemon/handlers/volume/zfs.rs`
- Test: `crates/ployz-runtime-backends/src/storage/zfs.rs`

**Approach:**

- Add a blocking participant `clone_volume`/`branch_volume` method.
- Generate deterministic deploy-scoped snapshot names.
- In daemon/ZFS code, create or reuse the source snapshot, clone it to the
  target dataset, set target quota/mode/owner, and verify target origin and
  source snapshot GUID.
- Return snapshot name/GUID and target dataset evidence to the executor.
- Execute clone work before starting candidates that mount the target volume.
- Fail deploy before commit if snapshot, clone, or verification fails.

**Test Scenarios:**

- Happy path: clone executes before start and commit includes evidence.
- Error path: clone failure does not start candidates and does not commit.
- Retry path: matching existing target clone can be adopted; mismatched target
  dataset rejects.
- Backend test: ZFS command shape uses `zfs snapshot`, `zfs clone`, and
  verification of `origin`/GUID.

### U5. Add Real ZFS E2E Scenario

**Goal:** Prove the primitive works as a user-visible deploy flow.

**Files:**

- Add: `crates/ployz-e2e/src/scenarios/volume_clone_branch_real_smoke.rs`
- Modify: `crates/ployz-e2e/src/scenarios/mod.rs`
- Modify: `crates/ployz-e2e/src/cli.rs`
- Modify: `crates/ployz-e2e/src/scenarios/zfs_support.rs` if helpers need
  clone-manifest support.

**Approach:**

- Deploy `prod/db` with a managed single-scope volume and write source data.
- Deploy `pr-39/db` with a volume clone intent from `prod/data`.
- Assert preview/apply report clone work.
- Verify `pr-39/db` sees source data at snapshot time.
- Mutate source and clone independently and assert divergence.
- Verify both real ZFS datasets exist and are mounted to the expected
  containers.

**Test Scenarios:**

- Real clone contains source data.
- Source writes after snapshot do not appear in target.
- Target writes do not mutate source.
- Reapply after successful clone is idempotent or reports no clone work rather
  than data loss.

## Verification

- `cargo fmt --check`
- `cargo test -p ployz-types`
- `cargo test -p ployz-store-api`
- `cargo test -p ployz-orchestrator`
- `cargo test -p ployz-runtime-backends --no-default-features`
- `cargo test -p ployzd volume_zfs`
- `cargo test -p ployz-e2e`
- `scripts/generate-deploy-types.sh`
- `just test-all`
- PR CI, including the new real-ZFS scenario.

## Risks

- ZFS clone is local. Cross-machine branch behavior must be rejected, not
  approximated with local clone vocabulary.
- Crash-consistent database snapshots can still require app-level recovery. v1
  must make that policy explicit rather than implying quiesced copies.
- Snapshot/clone may succeed before deploy commit fails. Retry and cleanup must
  treat provisional target datasets carefully and never delete a dataset unless
  it matches expected Ployz clone evidence.
- Clone evidence is public durable truth. It must not contain raw manifests or
  secrets.
- This changes public deploy schema, so generated package drift is likely if
  schema generation is skipped.
