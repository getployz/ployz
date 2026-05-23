---
title: "feat: Deploy image availability preflight"
type: feat
status: completed
date: 2026-05-11
origin:
  - VISION.md
  - docs/plans/2026-05-10-004-feat-core-build-image-availability-plan.md
  - docs/plans/2026-05-11-001-feat-image-push-existing-image-plan.md
---

# feat: Deploy image availability preflight

## Summary

Make deploy preview/apply consume the image availability evidence produced by
`image inspect`, `image push`, and `image distribute`. For services that use
`pull_policy: never`, deploy must prove the required image digest is present on
each planned target machine before it mutates runtime state.

This closes the first correctness loop for core builds without adding hidden
deploy-time image movement. Operators still build, push, inspect, or distribute
as explicit commands; deploy only validates the evidence those commands wrote.

## Problem Frame

The core can now record per-machine image availability and move images through
explicit foreground operations. Deploy planning still treats images as runtime
references only. That leaves a production failure class: a user can request a
deploy with `pull_policy: never` and only discover a missing image when a target
runtime tries to start the container.

This slice moves that failure into the deploy preflight boundary. Missing image
availability becomes a structured deploy planning/apply failure, not a late
runtime start failure and not an implicit invitation for deploy to pull, build,
or distribute.

## Scope

In scope:

- Parse digest-pinned service image identities for services with
  `pull_policy: never`.
- Build deploy image availability preview evidence for service slots that will
  create or replace runtime containers.
- Require matching `ImagePresence::Present` records for each planned
  `(machine, digest)` before preview succeeds.
- Re-run the same preflight before apply participant inspection and before any
  deploy mutation.
- Preserve current behavior for `if_not_present` and `always` pull policies.
- Add unit and E2E coverage for successful pushed-image deploy and missing-image
  rejection.

Out of scope:

- No deploy-time image push, distribute, inspect, build, or pull fallback.
- No registry credential or pullability preflight for `if_not_present` or
  `always`.
- No platform/architecture matching.
- No image garbage collection.
- No cloud builder integration.
- No attempt to infer image presence from currently running containers.

## Requirements

- R1. `pull_policy: never` requires a bare digest image identity for slots that
  create or replace runtime containers. Mutable tags and repo-qualified digest
  references must be rejected with a structured error instead of becoming
  unchecked runtime work.
- R2. Deploy preview validates image availability for every create/replace slot
  planned on a concrete machine.
- R3. Deploy apply performs the same validation before participant inspection,
  phase execution, runtime start, stop, volume move, or commit mutation.
- R4. Availability is satisfied only by durable `ImageAvailabilityRecord`
  entries whose presence is `Present`; absent, transferring, failed, and missing
  records are blocking.
- R5. Successful preview includes typed image availability evidence so callers
  can render which service/slot/machine/digest was proven.
- R6. Registry pull policies remain unchanged and do not require availability
  records.
- R7. E2E proves a pushed image can be deployed with `pull_policy: never`.
- R8. E2E proves the same deploy shape fails cleanly when the image was not
  pushed to the target machine.

## Key Decisions

- **Deploy consumes evidence; it does not repair it.** Missing availability is a
  caller-facing deploy error. The remediation is an explicit `image push`,
  `image distribute`, or `image inspect`, not hidden deploy behavior.
- **Preflight lives below the daemon.** The orchestrator owns deploy plan
  validation, so CLI, daemon, SDK, and future cloud consumers all get the same
  behavior.
- **Check only create/replace slots.** Unchanged slots do not start new runtime
  containers and may predate image availability records. Blocking no-op deploys
  on missing historical records would make adoption noisy without improving the
  mutation boundary.
- **Digest identity is runtime-verifiable.** Deploy accepts the same bare
  `ImageDigest` key used by image availability records and passed to the
  runtime. For local image pushes this may be a Docker image id, matching the
  current runtime-verifiable identity contract. Repo-qualified digest references
  stay out of scope until availability records can prove that exact local
  runtime reference.
- **Preview and apply share one preflight function.** Divergence between preview
  and apply would recreate the same late-failure class this slice is removing.

## Implementation Units

### U1. Model Deploy Image Availability Evidence

**Goal:** Add a stable deploy preview payload that reports proven image
availability checks.

**Requirements:** R2, R4, R5, R6

**Files:**

- Modify: `crates/ployz-types/src/model.rs`
- Test: `crates/ployz-types/src/model.rs`

**Approach:**

- Add `DeployImageAvailabilityPlan` with service, slot id, machine id, image
  reference, digest, and status.
- Add a small status enum that starts with `present`; keep missing/failed states
  out of successful preview if preview fails on them.
- Add `DeployPreview.image_availability` with default/empty serde behavior for
  old payloads.
- Add serde tests for the new field and for backward-compatible legacy preview
  deserialization.

**Test Scenarios:**

- Preview serializes `image_availability` entries with snake_case status values.
- Legacy preview JSON that lacks `image_availability` still deserializes.

### U2. Add Shared Deploy Image Preflight

**Goal:** Resolve and validate required image availability for planned deploy
slots.

**Requirements:** R1, R2, R3, R4, R5, R6

**Files:**

- Modify: `crates/ployz-types/src/error.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/plan.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/mod.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/execute.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**

- Parse required digests from service images only when
  `template.pull_policy == PullPolicy::Never` and at least one slot will create
  or replace a runtime container.
- Accept digest references only in bare `sha256:...` form.
- Reject non-digest and repo-qualified `pull_policy: never` images with a
  structured deploy error.
- For each create/replace slot, read `store.get_image_availability(machine,
  digest)`.
- Return a structured deploy error for missing records or non-present presence.
- Include successful checks in `DeployPreview.image_availability`.
- Call the preflight from both `preview` and
  `apply_with_deploy_id_and_preconditions` before participant reachability and
  execution support checks.
- Preserve `if_not_present` and `always` behavior by returning no required
  checks for those services.

**Test Scenarios:**

- Preview succeeds and includes one image availability record when a planned
  `pull_policy: never` service has a present image on its target.
- Preview rejects a `pull_policy: never` mutable tag before runtime mutation.
- Preview rejects a missing image availability record with service, slot,
  machine, image, and digest context.
- Preview rejects an absent/transferring/failed record.
- Preview ignores `if_not_present` and `always` services.
- Apply rejects missing image availability before participant inspect/start.

### U3. Add Host-Runtime E2E Coverage

**Goal:** Prove the user-facing image push to deploy flow against the real E2E
harness.

**Requirements:** R7, R8

**Files:**

- Modify: `crates/ployz-e2e/src/cli.rs`
- Modify: `crates/ployz-e2e/src/scenarios/mod.rs`
- Create: `crates/ployz-e2e/src/scenarios/deploy_image_availability.rs`
- Test: `crates/ployz-e2e/src/scenarios/deploy_image_availability.rs`

**Approach:**

- Add a two-node host-runtime scenario.
- Initialize a mesh and add the peer.
- Resolve the local Docker image id for the preloaded smoke image on founder.
- First apply a deploy with `pull_policy: never` targeting the peer without
  pushing the image and assert the deploy command fails with image availability
  context.
- Push the same image to the peer with `image push`.
- Re-apply the deploy and assert the service starts on peer.

**Test Scenarios:**

- Missing image availability fails before service start.
- Pushed image availability lets deploy proceed with `pull_policy: never`.
- The scenario is included in the default non-ZFS E2E set so CI continuously
  exercises the preflight.

## Verification

- `cargo fmt --check`
- `cargo test -p ployz-types`
- `cargo test -p ployz-orchestrator`
- `cargo test -p ployz-e2e`
- `cargo run -p ployz-e2e -- --scenario deploy_image_availability --fail-fast`
- `just test-all`
- PR CI, including the new E2E scenario

## Risks

- Docker image ids are valid runtime identities but less conventional than
  registry manifest digests. Tests should make the supported reference forms
  explicit.
- Existing deployments with `pull_policy: never` and mutable tags continue to
  preview/apply when unchanged, but will fail earlier when they create or
  replace containers. That is intentional for correctness, but the error must be
  actionable.
- Requiring availability for unchanged slots would block adoption on historical
  deploys that predate image records, so this slice intentionally limits checks
  to create/replace work.
- A final runtime race remains possible if an image is removed after coordinator
  preflight and before node start. Docker still enforces `pull_policy=never`,
  but returning an image-specific node failure should be a follow-up deploy-node
  contract slice.
