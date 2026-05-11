---
title: "feat: Image push existing image"
type: feat
status: completed
date: 2026-05-11
origin: docs/plans/2026-05-10-008-feat-single-target-image-distribute.md
---

# feat: Image push existing image

## Summary

Implement `ployzd image push IMAGE --to ...` for an image that already exists
on the caller's local runtime. The command should verify the source image,
transfer/import it to the first target through the existing receiver path, then
optionally distribute from that cluster source to additional targets.

This is the bridge between "I built this image somewhere" and "Ployz knows
which machines can run it." Dockerfile, Railpack, and Cloud builders can later
produce the same source image and call this primitive instead of defining their
own image movement behavior.

---

## Problem Frame

The API and CLI already expose `ImagePushRequest`, but the daemon handler still
returns `IMAGE_PUSH_UNIMPLEMENTED`. `image distribute` now proves the
machine-to-machine transfer/import loop for one target. The next useful slice is
to make the operator/workstation entrypoint real while keeping deploys free from
hidden build or transfer work.

---

## Assumptions

*This plan is written in LFG pipeline mode without synchronous confirmation.*

- The branch is stacked on `codex/single-target-image-transfer` because this
  slice depends on the receiver/import/distribute implementation from PR #166.
- The first implementation may execute multi-target push serially. Throughput
  tuning and target concurrency are follow-up work.
- A push source is the local daemon runtime. Pushing from an arbitrary external
  workstation process without a local daemon remains a future CLI/build flow.
- Existing Docker archive export/import support is the runtime primitive for
  this slice.

---

## Requirements

- R1. `ImagePushRequest` executes instead of returning
  `IMAGE_PUSH_UNIMPLEMENTED`.
- R2. Push rejects zero targets before creating operation side effects.
- R3. The local runtime source image is verified before transfer starts. If
  `expected_digest` is provided, it must match the runtime image; otherwise the
  handler must resolve a verified digest from the source image.
- R4. The first target is imported through the existing receive-session,
  archive upload, target import, and digest verification path.
- R5. Additional targets, if requested, are reached by distributing from the
  first successfully imported cluster source, not by treating the workstation as
  a cluster distribute source for every target.
- R6. Push returns an `ImagePushPayload` with one artifact, one operation id,
  and per-target outcomes.
- R7. Operation records preserve visible failure status and per-target errors;
  failed imports or verification must not record `Present` availability.
- R8. E2E coverage exercises the CLI flow against the host-runtime harness:
  build or seed a local image, push it to a peer, and verify recorded image
  availability through `image status`.

---

## Scope Boundaries

- No Dockerfile build command.
- No Railpack integration.
- No Cloud builder pool or Cloud-specific scheduling.
- No deploy preflight integration in this slice.
- No long-lived registry product, registry credentials, or Docker daemon
  insecure-registry configuration.
- No hidden deploy-time push/distribute.

### Deferred to Follow-Up Work

- Deploy image availability preflight for `PullPolicy::Never`.
- Local Dockerfile/Railpack build commands that end by calling `image push`.
- Selected-machine source bundle build.
- Parallel fanout and resumable partial push.

---

## Context & Research

### Relevant Code and Patterns

- `crates/ployz-api/src/image.rs` already defines `ImagePushRequest` and
  `ImagePushPayload`.
- `crates/ployzd/src/cli.rs` and `crates/ployzd/src/request_builder.rs` already
  parse/build `image push`.
- `crates/ployzd/src/daemon/handlers/image/push.rs` owns push, distribute,
  receive-session, and received-import handlers.
- `crates/ployzd/src/daemon/handlers/image/archive.rs` already parses Docker
  archives, uploads blobs to the receiver with `HEAD` skips, and reconstructs
  received archives.
- `crates/ployzd/src/daemon/handlers/image/operations.rs` owns durable image
  operation records.
- `crates/ployz-e2e/src/scenarios/` contains host-runtime scenarios that run
  real CLI commands inside node containers.

### Institutional Learnings

- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`
  applies directly: receiver blobs are transfer cache, while verified runtime
  image presence is durable availability truth.
- `VISION.md` keeps this command-shaped: image movement is an explicit
  foreground operation, not deploy reconciliation.

---

## Key Technical Decisions

- **Reuse the distribute target executor shape.** Push and distribute should not
  maintain two different archive upload/import paths. If implementation reveals
  duplication, extract a narrow helper in `push.rs` that handles one target
  transfer from an already-exported archive.
- **First target becomes the cluster source.** Multi-target push imports to the
  first target, then calls the existing distribute path from that first target
  to each remaining target. This preserves the invariant that `image
  distribute` sources are cluster machines.
- **Runtime-verifiable artifact identity.** `ImagePushPayload.artifact` should
  describe the verified runtime image identity and optional repository/tag parsed
  from the pushed reference. Docker archive import preserves image ids even when
  repository digests are absent, so existing-image push prefers the runtime image
  id and accepts repository digests as an explicit expectation. Deploy preflight
  will consume the same runtime-verifiable identity later.
- **Serial fanout for now.** Serial execution is simpler and keeps failure
  ordering legible. Per-target outcomes still prepare the surface for later
  concurrency.

---

## Implementation Units

### U1. Push Source Verification and Single-Target Import

**Goal:** Replace the push stub with a real single-target path from local
runtime image to target availability.

**Requirements:** R1, R2, R3, R4, R6, R7

**Dependencies:** None

**Files:**

- Modify: `crates/ployzd/src/daemon/handlers/image/push.rs`
- Test: `crates/ployzd/src/daemon/handlers/image/push.rs`

**Approach:** Validate at least one target and active mesh before creating an
operation. Resolve/verify the source image digest from `source_image` plus
`expected_digest` when present. Export and parse the local runtime archive,
open a receive session for the first target, upload missing blobs, ask the
target to import the received artifact, and record success or failure through
the image operation store.

**Patterns to Follow:** Mirror the failure handling and cleanup behavior from
`handle_image_distribute_with_backend`. Preserve explicit operation status
updates and do not swallow persistence errors.

**Test Scenarios:**

- Happy path: pushing `example/app:latest` to the local/self target imports the
  archive, records `Present`, returns `ImagePushPayload`, and cleans transfer
  work directories.
- Edge case: zero targets returns a push-specific error and leaves the image
  operation store empty.
- Error path: source digest verification failure marks the push operation
  failed and records no availability.
- Error path: target import or verification failure returns failure and records
  no `Present` availability.
- Error path: receive-session failure marks the operation failed with a visible
  target error.

**Verification:** Unit tests prove the handler mutates operation and
availability state only after digest verification.

### U2. Multi-Target Push Fanout Through Cluster Source

**Goal:** Let `image push` accept multiple targets while preserving the rule
that further distribution starts from a machine that already has the image.

**Requirements:** R5, R6, R7

**Dependencies:** U1

**Files:**

- Modify: `crates/ployzd/src/daemon/handlers/image/push.rs`
- Test: `crates/ployzd/src/daemon/handlers/image/push.rs`

**Approach:** Treat the first requested target as the import target. For each
remaining target, invoke the same one-target distribute logic using the first
target as the source. Aggregate per-target results in the push payload and
operation record. If a later target fails, return an operator-visible partial
failure rather than pretending the whole push succeeded.

**Patterns to Follow:** Reuse `ImageTransferTargetResult` and
`ImageOperationTargetOutcome` semantics already used by distribute.

**Test Scenarios:**

- Happy path: pushing to two targets records `Present` for both targets and
  returns two target results.
- Error path: first target failure prevents fanout and records all attempted
  failure state visibly.
- Error path: later target failure preserves first target success and returns a
  partial-failure result.

**Verification:** Tests show push fanout never records availability for a
target that failed import or verification.

### U3. CLI Rendering and Operation Visibility

**Goal:** Ensure the existing CLI request surface produces useful output for
push success and failure.

**Requirements:** R6, R7

**Dependencies:** U1

**Files:**

- Modify: `crates/ployzd/src/cli_io.rs`
- Test: `crates/ployzd/src/cli_io.rs`
- Test: `crates/ployzd/src/main.rs`

**Approach:** Reuse the existing daemon response renderer when possible. Add
  push-specific plain output only if the generic payload output is not adequate
  for target status and artifact digest visibility.

**Patterns to Follow:** Match existing `image status`, `image inspect`, and
operation list/get output conventions.

**Test Scenarios:**

- Happy path: plain output includes the pushed digest and target machine.
- Error path: failed response surfaces the daemon error code/message without
  hiding target failure details.
- Existing request-builder tests for `image push` remain valid.

**Verification:** CLI tests cover parse/build/render behavior without changing
the command contract.

### U4. E2E Push Existing Image Scenario

**Goal:** Prove the real CLI path with the host-runtime E2E harness.

**Requirements:** R8

**Dependencies:** U1, U2, U3

**Files:**

- Modify: `crates/ployz-e2e/src/cli.rs`
- Modify: `crates/ployz-e2e/src/scenarios/mod.rs`
- Create: `crates/ployz-e2e/src/scenarios/image_push_existing_image.rs`
- Modify: `crates/ployz-e2e/src/support.rs`
- Modify: `crates/ployz-e2e/src/runner.rs`

**Approach:** Add an off-ZFS two-node scenario. Start a mesh, create or reuse a
small local Docker image inside the founder runtime, run `ployzd image push`
from founder to peer, then run `ployzd --json image status --machine peer` and
assert the digest is recorded as present.

**Patterns to Follow:** Follow `mesh_bootstrap_join_smoke.rs` for two-node
setup and `support.rs` JSON parsing helpers for daemon responses.

**Test Scenarios:**

- Happy path: E2E pushes a known image to peer and observes peer availability.
- Error path coverage remains in unit tests; the E2E should stay focused and
  cheap enough for CI.

**Verification:** The new scenario is runnable directly and appears in the CI
scenario list for off-ZFS runs.

---

## System-Wide Impact

- **Operators:** gain the first usable "make this existing image available"
  primitive.
- **Future build flows:** Dockerfile, Railpack, and Cloud builders can all end
  with the same `image push` request.
- **Deploy planning:** no deploy behavior changes yet; later preflight can read
  availability records produced here.
- **Runtime/storage:** receiver CAS remains temporary transfer state; runtime
  image verification remains the source of availability truth.

---

## Risk Analysis & Mitigation

- **Digest resolution ambiguity:** if the runtime cannot resolve a digest from
  a tag-only source, fail loudly rather than recording weak availability.
- **Partial multi-target semantics:** return and store per-target outcomes so
  operators can see exactly which targets need retry.
- **E2E cost:** keep the scenario off-ZFS and focused on image push/status, not
  deploy, to avoid making CI materially heavier.
- **Duplication with distribute:** prefer small helpers in `push.rs` over a
  broad abstraction; preserve behavior with focused handler tests.

---

## Validation Plan

- `cargo fmt --check`
- `cargo test -p ployz-api --no-default-features image_push`
- `cargo test -p ployzd --no-default-features image_push`
- `cargo test -p ployz-e2e --no-default-features`
- Run the new E2E scenario directly before PR.
- Run full repository test suite before opening the PR if the implementation
  touches shared request/daemon/e2e surfaces.
