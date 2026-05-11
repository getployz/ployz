---
title: "feat: Local build image availability"
type: feat
status: active
date: 2026-05-11
origin:
  - VISION.md
  - docs/plans/2026-05-10-004-feat-core-build-image-availability-plan.md
  - docs/plans/2026-05-11-001-feat-image-push-existing-image-plan.md
  - docs/plans/2026-05-11-002-feat-deploy-image-availability-preflight.md
---

# feat: Local build image availability

## Summary

Add the first usable local build primitive: an operator can run a Dockerfile or
Railpack build on the local daemon machine, get a digest-pinned artifact, and
record `Present` image availability for the local machine. The existing
`image push` and deploy image preflight primitives then move and consume that
artifact without deploy doing hidden build work.

This keeps the core product focused on explicit commands. Local build produces
evidence; image push moves it; deploy validates it.

## Problem Frame

The core now has image availability records, image push/distribute, and deploy
preflight, but the user still needs to produce the local image outside Ployz.
That means the primary workflow we want for open core still has a manual gap:
build on my machine, push to my server, deploy with `pull_policy: never`.

This slice closes that gap without adding cloud builders, build scheduling, or
selected-machine source bundle builds. External, non-cluster builders are a
separate ingress primitive: they should upload/import into a connected cluster
node, and availability should be recorded for that receiving node rather than
for the external machine.

## Requirements

- R1. Add a local build request and CLI command that supports
  `BuildMethod::Dockerfile` and `BuildMethod::Railpack`.
- R2. A successful local build records a `BuildOperationRecord` with
  `BuildOperationKind::Local`, method, `BuildLocation::Local`, stage, and the
  resulting `ImageArtifact`.
- R3. A successful local build records `ImageAvailabilityRecord::Present` for
  `(local_machine_id, digest)`.
- R4. Build output must include the operation id, artifact, and local
  availability record so agents and scripts can use the digest directly.
- R5. Build failure must be visible in the daemon response and durable build
  operation record.
- R6. Dockerfile builds must run against a caller-provided context directory and
  image name.
- R7. Railpack builds must use Railpack's CLI build contract:
  `railpack build --name IMAGE [--platform PLATFORM] DIRECTORY`.
- R8. No automatic push, distribute, or deploy is performed by local build in
  this slice.
- R9. E2E proves the operator flow: build locally, push to peer, deploy the
  digest with `pull_policy: never`.

## Scope

In scope:

- `ployzd build local --method dockerfile|railpack --image IMAGE CONTEXT`.
- Optional `--platform os/arch[/variant]` parsing and propagation to the build
  command.
- Local build operation list/get reuse through existing build operation APIs.
- Dockerfile and Railpack command execution on the daemon host.
- Runtime inspection after build to derive and validate the artifact digest.
- Local availability recording with build provenance.
- Unit tests for request building, handler success/failure, and operation
  persistence.
- One host-runtime E2E using Dockerfile build, image push, and deploy preflight.

Out of scope:

- No selected-machine build.
- No external non-cluster builder push/upload ingress.
- No source bundle upload.
- No cloud builder pool.
- No build cache UX.
- No automatic push/distribute after build.
- No build progress streaming.
- No deploy-time build fallback.

## Key Decisions

- **Build is a foreground primitive.** The command runs, returns a result, and
  records an operation. It does not create a background worker or scheduler.
- **Post-build runtime inspection is authoritative.** The build command may tag
  an image, but availability is recorded only after the runtime can inspect the
  built image and provide a valid `ImageDigest`.
- **Use local tools before inventing a build engine abstraction.** Dockerfile
  builds use Docker's local build command. Railpack builds use the official
  CLI shape from Railpack docs. A future performance slice can move Dockerfile
  builds to Bollard/BuildKit if needed.
- **No hidden image movement.** Build stops after local availability. Existing
  `image push` remains the explicit movement primitive.
- **Digest remains the deploy identity.** The CLI may ask for an image name/tag,
  but the successful payload tells the operator which bare digest to use with
  `pull_policy: never`.

## Implementation Units

### U1. Request and CLI Surface

**Goal:** Expose a local build command that maps to a typed daemon request.

**Requirements:** R1, R4, R6, R7

**Files:**

- Modify: `crates/ployz-api/src/request.rs`
- Modify: `crates/ployzd/src/cli.rs`
- Modify: `crates/ployzd/src/request_builder.rs`
- Modify: `crates/ployzd/src/main.rs`
- Test: `crates/ployz-api/src/request.rs`
- Test: `crates/ployzd/src/main.rs`

**Approach:**

- Add `DaemonRequest::BuildLocal { request: BuildLocalRequest }`.
- Add top-level `build local` CLI with `--method`, `--image`, optional
  `--platform`, and context directory argument.
- Reuse existing `BuildLocalRequest` from `crates/ployz-api/src/build.rs`;
  leave its push/distribute fields empty because this slice does not move
  images automatically.
- Extend request-lane and request dispatch to route local build through the
  shared lane.
- Reject overlapping same-image local builds with a per-daemon image-name lock
  so long builds do not block read-only daemon requests, while concurrent
  builds cannot race on the same mutable Docker tag. Untagged image names are
  normalized to `:latest` before locking and building.

**Test Scenarios:**

- Parse/build `build local --method dockerfile --image app:ployz .` into
  `DaemonRequest::BuildLocal`.
- Parse/build `build local --method railpack --platform linux/amd64 --image app:ployz .`
  with the platform preserved.
- Invalid platform is rejected by request builder before daemon transport.

### U2. Local Build Execution and Availability Recording

**Goal:** Execute local Dockerfile/Railpack builds, inspect the output image,
and persist build and availability evidence.

**Requirements:** R2, R3, R4, R5, R6, R7, R8

**Files:**

- Modify: `crates/ployzd/src/daemon/handlers/build.rs`
- Create: `crates/ployzd/src/daemon/handlers/build/local.rs`
- Modify: `crates/ployzd/src/daemon/handlers/build/operations.rs`
- Modify: `crates/ployzd/src/daemon/handlers/mod.rs`
- Test: `crates/ployzd/src/daemon/handlers/build/local.rs`
- Test: `crates/ployzd/src/daemon/handlers/build/operations.rs`

**Approach:**

- Add a local build handler that creates a running `BuildOperationRecord`,
  executes the selected build command, inspects `image_name` with
  `runtime_image_backend`, builds an `ImageArtifact` with
  `ImageArtifactProvenance::Build { method, location: Local, source_digest:
  None }`, and records local `ImageAvailabilityRecord::Present`.
- Dockerfile command shape: `docker build -t IMAGE [--platform PLATFORM] CONTEXT`.
- Railpack command shape, from official Railpack CLI docs:
  `railpack build --name IMAGE [--platform PLATFORM] CONTEXT`.
- On command failure, persist the operation as `Failed` with the captured
  stderr/stdout summary and return a daemon error.
- Keep command execution behind a tiny internal runner seam so handler tests can
  fake success/failure without running Docker or Railpack.

**Test Scenarios:**

- Dockerfile success persists succeeded build operation and local image
  availability.
- Railpack success uses the expected command shape and persists Railpack
  provenance.
- Command failure marks operation failed and does not write availability.
- Runtime inspect returning no image after a successful command marks the build
  failed and does not write availability.
- Operation list/get includes local build records.

### U3. CLI Rendering and E2E

**Goal:** Make the command usable by humans and prove the complete local build
to push to deploy flow.

**Requirements:** R4, R9

**Files:**

- Modify: `crates/ployzd/src/cli_io.rs`
- Modify: `crates/ployz-e2e/src/cli.rs`
- Modify: `crates/ployz-e2e/src/scenarios/mod.rs`
- Create: `crates/ployz-e2e/src/scenarios/local_build_image_availability.rs`
- Test: `crates/ployzd/src/main.rs`
- Test: `crates/ployz-e2e/src/scenarios/local_build_image_availability.rs`

**Approach:**

- Add compact plain output for `BuildResultPayload`: operation id, digest,
  image name, local machine.
- Add a host-runtime E2E that writes a tiny Dockerfile context on founder,
  runs `ployzd --json build local --method dockerfile --image ployz-e2e-local-build:http .`,
  extracts the digest from the JSON payload, drains founder to force peer
  placement, proves deploy preflight rejects the missing peer availability,
  pushes the built image to peer, deploys a manifest with the digest and
  `pull_policy: never`, and waits for the peer service container.
- Keep Railpack out of E2E because CI runners may not have the Railpack binary;
  Railpack remains covered by command-shape unit tests.

**Test Scenarios:**

- Plain build output includes operation id and digest.
- E2E fails if local build does not record availability.
- E2E fails if deploy cannot consume the built digest after explicit push.

## Verification

- `cargo fmt --check`
- `cargo test -p ployz-api build_local`
- `cargo test -p ployzd --no-default-features daemon::handlers::build::local::tests`
- `cargo test -p ployzd --no-default-features build_local`
- `cargo test -p ployz-e2e --no-default-features local_build_image_availability`
- `cargo run -p ployz-e2e -- --scenario local_build_image_availability --fail-fast`
- `just test-all`
- PR CI

## Risks

- Docker CLI availability differs by runtime profile. The first implementation
  should return a structured command failure rather than pretending all hosts
  can build.
- Railpack may not be installed. That is a foreground build failure with an
  operation record, not a deploy failure.
- Shelling out is simpler than a full BuildKit API integration but less
  controllable. Keep the runner seam narrow so a later performance slice can
  replace execution without changing the daemon/API contract.
- Build context size can be large. This local-machine slice does not upload
  source, so context limits belong to the future selected-machine bundle build.
- External builder machines should not be modeled as mesh machines just because
  they produced an image. The future ingress path should make the receiving
  cluster node the availability owner.
