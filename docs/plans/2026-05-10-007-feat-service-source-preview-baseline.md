---
title: "feat: Service source preview and apply baseline"
type: feat
status: active
date: 2026-05-10
origin:
  - VISION.md
  - docs/architecture/deploy-primitives-roadmap.md
  - docs/plans/2026-05-10-006-feat-service-source-primitives.md
---

# feat: Service source preview and apply baseline

## Problem Frame

Deploy already resolves branch source lineage for `ServiceIntent::Branch` and
commits branch lineage with the target release. The missing slice is the public
preview contract: clients need a per-service source-mode table that distinguishes
fresh-derived services from branch-derived services, plus an apply baseline that
lets callers prove apply is using the same source revisions they previewed.

Without a preview baseline, apply can only compare plans resolved inside the
same apply call. It cannot reject drift that happened between the user's earlier
preview and apply request.

## Scope

In scope:

- Add typed service source-mode preview evidence to `DeployPreview`.
- Include both derived `fresh` services and explicit `branch` services.
- Keep existing `service_branch_sources` for lineage compatibility during this
  slice.
- Add a stable preview baseline/fingerprint derived from service source modes.
- Extend deploy apply options so callers can pass the expected preview baseline.
- Reject stale branch-source baseline before participant inspect/start RPCs.
- Preserve existing branch lineage commit behavior.

Out of scope:

- Full `ployzctl branch`.
- Portal source lookup or preview.
- Service move planning or preview.
- Cloud/dashboard consumption.
- Rust-to-TypeScript generation beyond touching schemas only if this slice
  changes generated manifest types.

## Key Decisions

1. `fresh` is preview evidence, not a manifest intent.
   Services with no supported source intent resolve to `fresh` with a derived
   origin such as `no_source_intent`. `ServiceIntent::Move` and
   `ServiceIntent::Portal` remain rejected by planning support before preview.

2. Branch source mode reuses committed source release truth.
   Branch mode should report source namespace, source service, and source
   revision hash. It should not replace durable branch lineage records yet.

3. Apply drift checks require caller-supplied preview baseline.
   The baseline belongs in API options or an equivalent apply request field, not
   in the manifest body. The manifest describes desired target state; the
   baseline is the caller's concurrency guard.

4. Baseline enforcement must run before participant work.
   If the expected branch source revision is stale, apply should fail before
   namespace participant inspection or start-candidate RPCs.

## Implementation Units

### U1. Model service source preview and baseline

Files:

- `crates/ployz-types/src/model.rs`
- `crates/ployz-types/src/error.rs`
- `crates/ployz-api/src/deploy.rs`

Approach:

- Add a `ServiceSourcePlan` preview model with an enum payload:
  - `fresh` with `origin = no_source_intent`,
  - `branch` with source namespace, source service, and source revision hash.
- Add `service_sources: Vec<ServiceSourcePlan>` to `DeployPreview`.
- Add `source_baseline` or equivalent deterministic fingerprint to
  `DeployPreview`.
- Add a deploy apply option for an expected service source baseline.
- Add a structured deploy error for expected baseline mismatch or stale branch
  source revision.

Test scenarios:

- `DeployPreview` serde serializes `fresh` and `branch` source modes with
  snake_case wire values.
- Missing `service_sources` in legacy preview JSON defaults to empty.
- Apply options serialize/deserialize with and without an expected baseline.

### U2. Populate preview source modes

Files:

- `crates/ployz-orchestrator/src/deploy/plan.rs`
- `crates/ployz-orchestrator/src/deploy/tests.rs`

Approach:

- Derive `fresh` source mode for services without a branch source.
- Derive `branch` source mode from existing `PlannedBranchSource`.
- Compute the baseline from the resolved service source-mode table in a stable
  order.
- Keep existing `service_branch_sources` populated for branch services.

Test scenarios:

- Fresh service preview shows `fresh` with `no_source_intent`.
- Branch service preview shows branch mode and the source revision hash.
- The preview baseline changes when the source branch revision changes.
- Portal and service move intents remain rejected before source-mode preview.

### U3. Enforce apply baseline before participant RPCs

Files:

- `crates/ployz-orchestrator/src/deploy/mod.rs`
- `crates/ployz-orchestrator/src/deploy/execute.rs`
- `crates/ployz-orchestrator/src/deploy/tests.rs`
- `crates/ployzd/src/daemon/handlers/deploy.rs`

Approach:

- Thread the optional expected baseline from daemon apply options into
  orchestrator apply.
- Compare the expected baseline with the initial resolved plan's baseline before
  unsupported execution guards and before `ParticipantSet::inspect`.
- Keep existing same-apply plan stability checks after lock/re-resolution.
- Leave apply without a baseline compatible with current behavior, but do not
  claim preview-to-apply drift protection for that path.

Test scenarios:

- Apply with matching expected baseline succeeds and commits branch lineage.
- Apply with stale expected baseline fails before participant inspect/start RPCs.
- Apply without expected baseline preserves current behavior.
- The branch lineage commit record still contains source namespace, service, and
  source revision hash.

## Verification

- `cargo test -p ployz-types service_source`
- `cargo test -p ployz-orchestrator service_source`
- `cargo test -p ployz-orchestrator branch_source`
- `cargo test -p ployzd deploy`
- `cargo check -p ployz-orchestrator`
- `cargo check -p ployzd`
- `cargo fmt --check`
- `just test-all` before final push if runtime is acceptable.
