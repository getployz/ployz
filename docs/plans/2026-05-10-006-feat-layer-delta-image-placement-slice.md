---
title: "feat: Layer-delta image placement foundation"
type: feat
status: active
date: 2026-05-10
origin: VISION.md
---

# feat: Layer-delta image placement foundation

## Problem Frame

Ployz needs an explicit image placement primitive so builds can happen away from
production traffic while deployment remains command-shaped and visible. This
foundation slice does not build images and does not complete remote
distribution. It prepares the open-core primitive surface for later local,
machine, and cloud builder flows by adding image placement contracts and the
target-side OCI receiver needed for layer-delta transfer.

The work must match the product direction in `VISION.md`: cloud builders are a
consumer of core primitives, not private core behavior; deploys must not hide
image transfer; and failures must be visible to the operator through structured
operation surfaces.

## Scope Boundaries

- No Dockerfile or Railpack build execution.
- No source-side proxy yet.
- No Docker runtime tag, push, pull, or untag backend yet.
- No complete `image push` or `image distribute` orchestration yet.
- No deploy preflight behavior changes.
- No durable user-facing registry product.
- No ployz-cloud builder fleet behavior.

## Requirements

- R1. Add public daemon request and response payload surfaces for explicit image
  push and image distribution.
- R2. Add CLI parsing and request-building for `image push IMAGE --to ...` and
  `image distribute --digest ... --from ... --to ...`.
- R3. Keep the command handlers routed but return explicit unimplemented errors
  until transport orchestration lands.
- R4. Add a target-side ephemeral OCI Distribution receiver primitive that
  streams blob bodies to disk rather than buffering full layers in memory.
- R5. Guard mutating receiver paths with Ployz session headers containing
  operation id, source machine id, and session token.
- R6. Store verified blobs in content-addressed `sha256` paths only after digest
  verification succeeds.
- R7. Preserve failed digest-mismatch uploads for diagnosis and retry rather
  than promoting them to CAS.
- R8. Cover request wire shapes, CLI request construction, platform validation,
  registry blob upload, digest mismatch, manifest storage, and session rejection
  with tests.

## Key Decisions

- **Use a narrow streamed receiver.** `ferro-oci-server` and
  `ferro-blob-store` are useful reference points, but their current body
  shapes buffer data. This slice should implement only the endpoint subset
  needed by Docker/OCI layer transfer and stream large bodies through files.
- **Make incomplete transport explicit.** `image push` and `image distribute`
  should be routable API commands, but must fail with clear unimplemented codes
  until the next slice wires runtime push/pull and proxy sessions.
- **Model multi-target requests now.** Later orchestration needs per-target
  outcomes and bounded target concurrency. The API should accept
  `target_machines` now instead of forcing a single-target contract that will
  churn immediately.
- **Keep availability truth separate from cache.** The receiver's blob CAS is
  transfer cache, not deploy truth. Availability records remain the source of
  verified per-machine image presence.

## Existing Patterns To Follow

- Request and payload enums in `crates/ployz-api/src/request.rs` and
  `crates/ployz-api/src/response.rs`.
- CLI enum and request-builder tests in `crates/ployzd/src/cli.rs`,
  `crates/ployzd/src/request_builder.rs`, and `crates/ployzd/src/main.rs`.
- Daemon request routing and metric names in
  `crates/ployzd/src/daemon/handlers/mod.rs` and `crates/ployzd/src/metrics.rs`.
- Image operation records in `crates/ployzd/src/daemon/handlers/image/operations.rs`.
- Transfer listener failure posture in
  `crates/ployzd/src/daemon/handlers/volume/transfer_listener.rs`.

## Implementation Units

### U1. Image Push/Distribute Contracts And CLI

**Goal:** Add the stable command/API surface for image placement without
claiming transport support is complete.

**Files:**

- Modify: `crates/ployz-api/src/image.rs`
- Modify: `crates/ployz-api/src/request.rs`
- Modify: `crates/ployz-api/src/response.rs`
- Modify: `crates/ployzd/src/cli.rs`
- Modify: `crates/ployzd/src/request_builder.rs`
- Modify: `crates/ployzd/src/daemon/handlers/image.rs`
- Create: `crates/ployzd/src/daemon/handlers/image/push.rs`
- Modify: `crates/ployzd/src/daemon/handlers/mod.rs`
- Modify: `crates/ployzd/src/metrics.rs`
- Test: `crates/ployz-api/src/request.rs`
- Test: `crates/ployzd/src/main.rs`

**Approach:**

- Change `ImagePushRequest` to carry `source_image`, `target_machines`,
  optional `platform`, and optional `expected_digest`.
- Keep `ImageDistributeRequest` digest/source/targets and add optional
  `platform`.
- Change `ImagePushPayload` to return `artifact` and per-target results.
- Add `DaemonRequest::ImagePush` and `DaemonRequest::ImageDistribute`.
- Add matching `DaemonPayload` variants.
- Add `image push` and `image distribute` CLI actions.
- Parse `os/arch` and `os/arch/variant`; reject empty variant or malformed
  platform strings.
- Route handlers through the shared lane and return explicit
  `IMAGE_PUSH_UNIMPLEMENTED` / `IMAGE_DISTRIBUTE_UNIMPLEMENTED` responses.

**Test Scenarios:**

- `ImagePushRequest` round-trips source image, multiple targets, and expected
  digest.
- `ImageDistributeRequest` round-trips digest, source machine, targets, and
  platform field.
- `image push` CLI parses image, two targets, platform, and expected digest.
- `image distribute` CLI parses digest, source, and target.
- Request builder rejects an empty platform variant such as `linux/amd64/`.

### U2. Streamed Ephemeral OCI Receiver

**Goal:** Add a reusable target-side registry receiver primitive with
session-gated writes and disk-backed CAS storage.

**Files:**

- Create: `crates/ployzd/src/daemon/handlers/image/registry.rs`
- Modify: `crates/ployzd/src/daemon/handlers/image.rs`
- Modify: `crates/ployzd/Cargo.toml`
- Modify: `Cargo.toml`
- Test: `crates/ployzd/src/daemon/handlers/image/registry.rs`

**Approach:**

- Add `ImageRegistry` with a root path, active sessions, and upload state.
- Expose an `axum` router for `/v2/`, blob upload/read/head, and manifest
  put/read/head.
- Require Ployz-only headers on mutating paths:
  `x-ployz-image-operation`, `x-ployz-source-machine`,
  `x-ployz-image-session`.
- Stream upload chunks from `Body::into_data_stream()` to temp upload files.
- Verify `sha256:` digest by streaming the upload file before moving it into
  `blobs/sha256/<prefix>/<suffix>`.
- Keep manifests small and bounded with a size limit.
- Return registry-shaped errors for auth, digest, and unsupported path failures.

**Test Scenarios:**

- Blob upload via start, patch, and finish stores bytes at the verified
  content-addressed path.
- Digest mismatch returns a structured error and leaves the temp upload in
  place without promoting a CAS blob.
- Manifest write/read returns a digest header and persisted body.
- Missing session headers reject mutating requests.

## Verification

- `cargo fmt`
- `cargo test -p ployz-api --no-default-features`
- `cargo test -p ployzd --no-default-features`

## Follow-Up Slices

- Add Docker runtime tag/push/pull/untag methods.
- Add source-side localhost proxy and target receive-session RPCs.
- Implement `image push` orchestration with per-target operation outcomes.
- Implement `image distribute` from a cluster source machine.
- Add Docker interop/e2e coverage for real layer skip behavior.
