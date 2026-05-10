---
title: "feat: Deploy preview baseline envelope"
type: feat
status: active
date: 2026-05-10
origin:
  - VISION.md
  - docs/plans/2026-05-10-007-feat-service-source-preview-baseline.md
---

# feat: Deploy preview baseline envelope

## Summary

Generalize the service-source-specific preview/apply guard into a typed deploy
baseline envelope. Preview should return one baseline object, apply should
accept one expected baseline object, and baseline drift should be rejected before
participant work with structured mismatch details.

This keeps deploy explicit and command-shaped: the caller previews a concrete
plan, carries that plan's baseline into apply, and gets a clear failure if the
cluster inputs changed before mutation.

## Problem Frame

The previous slice added `service_source_fingerprint` and an apply option for
that one source of preview-to-apply drift. That solves branch source drift, but
it creates the wrong long-term shape: every new deploy primitive could add its
own `expected_*` field, daemon normalization, failure payload fields, and
same-apply stability special case.

The deploy engine already resolves a broader deterministic plan: manifest hash,
participants, services, volumes, phases, service source evidence, volume moves,
and volume clone work. The next slice should expose a durable baseline envelope
for the preview/apply contract while keeping the existing detailed preview
tables for human and client inspection.

## Requirements

- `DeployPreview` must expose a first-class `baseline` object.
- The baseline must include the currently-enforced service source fingerprint.
- The baseline must also cover deterministic deploy inputs that already define
  the resolved plan contract: manifest, participants, phases/work, services,
  volumes, volume moves, and volume clones.
- `DeployOptions` must accept one expected baseline object for guarded apply.
- Apply must reject expected baseline drift immediately after initial plan
  resolution and before participant probes, inspect RPCs, start RPCs, volume
  transfer RPCs, or certificate work.
- Baseline mismatch failures must expose structured expected/actual details and
  identify which baseline component changed.
- Unguarded apply must preserve existing behavior: same-apply drift still fails
  through the existing execution-plan stability contract, not through preview
  baseline mismatch.
- Existing service source preview evidence and branch lineage behavior must be
  preserved.

## Assumptions

- The public API can replace the service-source-only apply field with the typed
  baseline object because the project is still greenfield and compatibility
  shims are not required for real deployments.
- The old `service_source_fingerprint` preview field can remain during this
  slice as a convenience/readability field, but the authoritative apply guard is
  the new `baseline`.
- Baseline component fingerprints should be opaque strings, not client-rebuilt
  algorithms. Clients should round-trip preview output into apply.
- Component-level mismatch details are enough for this slice; full structural
  diffs belong in later diagnostics if needed.

## Scope

In scope:

- Add deploy baseline model types under `crates/ployz-types/src/model.rs`.
- Populate the baseline from `ResolvedPlan` in
  `crates/ployz-orchestrator/src/deploy/plan.rs`.
- Replace service-source-only apply preconditions with baseline preconditions in
  orchestrator and daemon apply paths.
- Return structured baseline mismatch details through `ployz-api` daemon
  failure payloads.
- Update tests that construct `DeployPreview` literals.
- Add focused unit/orchestrator/daemon tests for guarded and unguarded behavior.

Out of scope:

- `ployzctl` UX for preview/apply baseline round-tripping.
- Cloud/dashboard consumption.
- Full structural diff rendering for baseline mismatches.
- Route/hostname ownership fingerprinting beyond the current resolved deploy
  plan. Hostname validation remains a separate pre-mutation check.
- Removing all legacy service-source-only preview fields.

## Key Decisions

1. Baseline is a typed envelope, not a single global hash only.
   A single `fingerprint` is useful for compact equality, but component
   fingerprints make failures actionable without parsing plan internals.

2. Baseline belongs in apply options, not the manifest.
   The manifest describes desired target state. The baseline is the caller's
   concurrency guard proving the apply request still refers to the previewed
   resolved plan.

3. The baseline is computed by the orchestrator.
   Clients should not need to know the hash algorithm or reconstruct plan input
   ordering. They should pass the preview baseline back unchanged.

4. Explicit baseline mismatch is a different failure from unguarded plan drift.
   Guarded preview-to-apply drift returns baseline mismatch. Unguarded apply
   keeps returning the existing execution-plan changed failure when the plan
   changes inside apply.

5. Baseline validation happens before all participant work.
   The point of the guard is to fail before remote calls and mutation when the
   caller's preview is stale.

## Existing Patterns To Follow

- `crates/ployz-types/src/model.rs` already defines typed deploy preview
  evidence, preview serde defaults, and stable fingerprint helpers.
- `crates/ployz-orchestrator/src/deploy/plan.rs` owns resolved-plan projection
  into `DeployPreview`; baseline construction belongs there.
- `crates/ployz-orchestrator/src/deploy/execute.rs` already splits initial
  precondition checks from post-lock plan stability checks.
- `crates/ployzd/src/daemon/handlers/deploy.rs` already converts API deploy
  options into orchestrator apply preconditions and wraps structured deploy
  failures.
- `docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md`
  reinforces that deploy should convert stored operator intent into explicit,
  previewable work at decision time rather than silently reconciling later.

## Implementation Units

### U1. Add DeployPreviewBaseline model

Files:

- `crates/ployz-types/src/model.rs`
- `crates/ployz-api/src/deploy.rs`

Approach:

- Add a `DeployPreviewBaseline` model with an overall fingerprint plus named
  component fingerprints.
- Include components for manifest, participants, phases, services, service
  sources, volumes, volume moves, and volume clones.
- Add `baseline: Option<DeployPreviewBaseline>` or an equivalent skipped-empty
  field to `DeployPreview`.
- Add `expected_baseline: Option<DeployPreviewBaseline>` to `DeployOptions`.
- Update deploy failure payloads to include expected and actual baselines plus a
  changed component list.

Test scenarios:

- `DeployPreviewBaseline` serializes with stable snake_case fields and
  round-trips.
- Empty legacy preview JSON still deserializes.
- `DeployOptions` round-trips with and without `expected_baseline`.
- Baseline mismatch payload serializes expected/actual baselines and changed
  component names.

### U2. Populate baseline from resolved plans

Files:

- `crates/ployz-orchestrator/src/deploy/plan.rs`
- `crates/ployz-orchestrator/src/deploy/tests.rs`

Approach:

- Add a resolved-plan helper that builds baseline components from existing plan
  data in deterministic order.
- Reuse the service source fingerprint as the service-source component.
- Derive component fingerprints from plan data that already participates in
  `PlanFingerprint`, avoiding separate ad hoc snapshots that can drift from
  execution semantics.
- Keep existing `service_source_fingerprint` populated from the new baseline
  component to avoid duplicate derivation.

Test scenarios:

- Preview baseline exists for normal service deploys.
- Reordering internal source tables does not change the baseline.
- Changing source revision changes only the service-source and overall baseline
  components expected for that drift.
- Changing participants changes the participant and overall baseline.
- Changing phase work changes the phase and overall baseline.
- Volume move and clone previews contribute to baseline components.

### U3. Enforce expected baseline before participant work

Files:

- `crates/ployz-orchestrator/src/deploy/execute.rs`
- `crates/ployz-orchestrator/src/deploy/mod.rs`
- `crates/ployz-orchestrator/src/deploy/tests.rs`

Approach:

- Replace `DeployApplyPreconditions.expected_service_source_fingerprint` with an
  expected baseline field.
- Validate expected baseline against the initial resolved plan before existing
  volume execution support checks, reachability probes, participant inspect, and
  mutation.
- Preserve post-lock `ensure_plan_stable` semantics: explicit expected baseline
  may return baseline mismatch when relevant, but unguarded applies continue to
  return `ExecutionPlanChanged` for same-apply drift.
- Keep baseline validation separate from final plan stability so pre-preview
  drift and same-apply drift remain distinguishable.

Test scenarios:

- Matching expected baseline succeeds.
- Stale expected baseline fails before participant inspect/start RPCs.
- Empty or structurally invalid expected baseline is rejected as an invalid
  deploy option.
- Unguarded apply with plan drift still returns `ExecutionPlanChanged`.
- Explicit expected baseline mismatch reports expected, actual, and changed
  components.

### U4. Thread baseline through daemon apply and failure payloads

Files:

- `crates/ployzd/src/daemon/handlers/deploy.rs`
- `crates/ployz-api/src/deploy.rs`

Approach:

- Convert `DeployOptions.expected_baseline` into orchestrator
  `DeployApplyPreconditions`.
- Reject malformed or empty expected baselines before mesh setup where possible.
- Map baseline mismatch errors to a specific daemon response code and structured
  deploy failure payload.
- Update existing service-source-only response fields to the generalized
  baseline fields.

Test scenarios:

- Daemon precondition extraction preserves non-empty expected baseline.
- Daemon rejects empty/malformed expected baseline before active mesh setup.
- Daemon wraps baseline mismatch with a specific response code and structured
  payload.
- Existing no-eligible-placement failure payload remains unchanged except for
  generalized optional baseline fields defaulting away.

## Verification

- `cargo test -p ployz-types baseline --quiet`
- `cargo test -p ployz-api deploy --quiet`
- `cargo test -p ployz-orchestrator baseline --quiet`
- `cargo test -p ployz-orchestrator service_source --quiet`
- `cargo test -p ployz-orchestrator branch_source --quiet`
- `cargo test -p ployzd baseline --quiet`
- `cargo test -p ployzd deploy --quiet`
- `cargo check -p ployz-orchestrator`
- `cargo check -p ployzd`
- `cargo fmt --check`
- `git diff --check`
- `just test`

## Deferred Follow-Up Work

- Add CLI preview/apply UX that saves and passes the baseline automatically.
- Add cloud/dashboard consumption and TypeScript bindings.
- Add richer structural diff diagnostics for changed components.
- Consider route/hostname ownership baseline coverage if managed route
  ownership needs preview-to-apply concurrency protection beyond current plan
  stability.
