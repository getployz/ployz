---
title: "feat: Image inspect availability"
type: feat
status: active
date: 2026-05-10
origin: docs/plans/2026-05-10-004-feat-core-build-image-availability-plan.md
---

# feat: Image inspect availability

## Summary

Add the first real image availability workflow: an explicit `image inspect`
command that probes runtime image presence and writes durable
`ImageAvailabilityRecord` evidence for the local machine. This turns the
foundation from the core build/image availability PR into a usable operator
primitive without starting image transfers or builds yet.

This follows `VISION.md`: image availability changes only when an operator runs
a command. Deploys still do not implicitly build, pull, distribute, or repair
images.

## Scope

In scope:

- Add `DaemonRequest::ImageInspect` and `DaemonPayload::ImageInspect`.
- Add `ployzd image inspect --digest <digest> [--reference <image-ref>]`.
- Inspect local runtime image presence through `RuntimeImageBackend`.
- Write `Present`, `Absent`, or `Failed` availability records for the local
  machine and requested digest.
- Return a typed `ImageInspectPayload` and compact human-readable output.
- Keep `image status` as the read-only view over durable records.

Out of scope:

- Remote machine fan-out inspection.
- `image push` and `image distribute`.
- Starting or wiring the image transfer listener into mesh lifecycle.
- Deploy image preflight.
- Dockerfile or Railpack builds.
- Background refresh loops or automatic image discovery.

## Requirements

- R1. Inspect is an explicit foreground command; it is the only thing in this
  PR that mutates image availability.
- R2. A digest-pinned reference must be accepted directly. A tag-only
  reference is only accepted when `--digest` supplies the availability key.
- R3. A local runtime image with the expected digest records `Present` with
  platform metadata when available.
- R4. A missing image records `Absent` for the local machine and digest.
- R5. Runtime/backend errors record `Failed` with caller-visible failure text.
- R6. Inspect must not rewrite unrelated machine/digest records.
- R7. Remote machine inspection must fail loudly until peer image RPC is
  implemented, not silently inspect the coordinator instead.

## Key Decisions

### Inspect Local First

The API already models multiple machines, but the first implementation should
only inspect the local machine. This keeps the PR small and avoids inventing
peer image RPC before the local runtime semantics are proven. A non-local
machine target returns a structured daemon error.

### Reference Defaults To Digest

`ImageInspectRequest` carries a digest. The runtime inspection reference should
default to the digest string, but the CLI can pass `--reference` for Docker
cases where the local daemon needs a repo/tag or repo@digest reference to find
the image.

### Runtime Errors Are Durable Evidence

`Absent` means the runtime answered "not found." Backend errors are not absence;
they become `Failed` records so later `image status` makes the failed inspect
visible without reading logs.

## Implementation Units

### U1. Wire Inspect API And CLI

**Files:**

- Modify: `crates/ployz-api/src/image.rs`
- Modify: `crates/ployz-api/src/request.rs`
- Modify: `crates/ployz-api/src/response.rs`
- Modify: `crates/ployz-sdk/src/lib.rs`
- Modify: `crates/ployzd/src/cli.rs`
- Modify: `crates/ployzd/src/request_builder.rs`
- Modify: `crates/ployzd/src/main.rs`
- Modify: `crates/ployzd/src/metrics.rs`

**Approach:**

- Add `ImageInspectRequest.reference: Option<String>` so operators can inspect a
  digest using a local image reference.
- Add `DaemonRequest::ImageInspect`.
- Add `DaemonPayload::ImageInspect`.
- Add `ployzd image inspect --digest <digest> [--reference <ref>] [--machine <id>]`.
- Keep multiple-machine fan-out out of CLI for now; a single optional machine
  is enough for local validation and future remote extension.

**Test Scenarios:**

- CLI parser accepts digest/reference/machine flags.
- Request builder rejects invalid digests before contacting the daemon.
- Request builder encodes inspect requests with digest, reference, and machine.
- API request/response JSON round-trips preserve the inspect shape.

### U2. Add Runtime Image Backend Access In Daemon

**Files:**

- Modify: `crates/ployzd/src/runtime_profile.rs`
- Modify: `crates/ployzd/src/daemon/runtime.rs`

**Approach:**

- Add a daemon helper that returns a `RuntimeImageBackend`.
- Docker/host profiles use `ContainerEngine::connect()`.
- Memory test profile returns an unsupported backend unless tests inject the
  pure inspect helper directly.

**Test Scenarios:**

- Memory runtime reports unsupported image backend capability instead of trying
  Docker.
- Docker feature path still compiles.

### U3. Implement Local Image Inspect Handler

**Files:**

- Create: `crates/ployzd/src/daemon/handlers/image/inspect.rs`
- Modify: `crates/ployzd/src/daemon/handlers/image.rs`
- Modify: `crates/ployzd/src/daemon/handlers/mod.rs`

**Approach:**

- Resolve the target machine: omitted target or local machine is allowed;
  non-local target returns `IMAGE_INSPECT_REMOTE_UNSUPPORTED`.
- Begin an image operation record with kind `Inspect`.
- Call a pure helper that inspects the runtime backend and maps the result to an
  availability record.
- Upsert the record through `ImageAvailabilityStore`.
- Mark the operation succeeded or failed, preserving last error on failure.
- Return `ImageInspectPayload`.

**Test Scenarios:**

- Present image records `Present` with artifact/platform and operation id.
- Missing image records `Absent`.
- Backend error records `Failed` and returns a daemon error with payload.
- Non-local target fails before inspecting runtime or mutating availability.
- Existing unrelated availability records remain unchanged.

### U4. Keep Status/Inspect Output Legible

**Files:**

- Modify: `crates/ployzd/src/daemon/handlers/image/status.rs`
- Modify: `crates/ployzd/src/daemon/handlers/image/inspect.rs`

**Approach:**

- Reuse the same compact line shape for status and inspect output:
  `<machine> <digest> <presence>`.
- Include operation id in inspect success/failure messages.

**Test Scenarios:**

- Inspect output includes machine id, digest, presence, and operation id.
- Status still reports persisted records after inspect writes them.

## Verification

- `cargo test -p ployz-api -p ployz-sdk`
- `cargo test -p ployzd --no-default-features image`
- `cargo test -p ployzd --no-default-features`
- `cargo test -p ployz-runtime-backends runtime::image_ref --features docker`

## Follow-Up Stack

- PR 2: remote `image inspect` fan-out through peer RPC.
- PR 3: `image push` and `image distribute` over the archive transfer listener.
- PR 4: deploy image availability preflight.
- PR 5: local and selected-machine Dockerfile/Railpack build modes.
