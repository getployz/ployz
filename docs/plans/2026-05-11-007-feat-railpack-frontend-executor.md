---
title: "feat: Railpack frontend executor"
type: feat
status: completed
date: 2026-05-11
origin:
  - VISION.md
  - docs/plans/2026-05-10-004-feat-core-build-image-availability-plan.md
  - docs/plans/2026-05-11-003-feat-local-build-image-availability.md
---

# feat: Railpack frontend executor

## Summary

Make Railpack a first-class local build path by replacing the unsafe
`railpack build` secret fallback with Railpack's documented prepare plus
BuildKit frontend flow. The core API remains `BuildMethod::Railpack`; the
daemon turns that into an explicit prepare command followed by a Docker
BuildKit build that can carry plain env, secret env, platform, image name, and
secret cache invalidation without leaking secret values through process
arguments or durable operation records.

This stays inside the open-core build primitive. It does not add cloud builders,
remote build pools, selected-machine source bundles, or hidden deploy-time
builds.

## Problem Frame

The current local build path supports Railpack only through
`railpack build --env KEY=VALUE`. That works for non-secret env but cannot
carry secret values safely because process arguments, debug rendering, and shell
history are easy leak surfaces. The previous secret-aware build-input slice
therefore validates and records Railpack secret input but rejects execution.

Railpack's production guidance solves this with a two-step model: generate a
plan using `railpack prepare`, then execute that plan with the Railpack BuildKit
frontend. Secrets are named during prepare, passed as BuildKit `--secret`
mounts during the Docker build, and cache invalidation is driven by a
`secrets-hash` build arg. This matches Ployz's primitive style: explicit
foreground commands, visible failure, no background policy.

## Requirements

- R1. `BuildMethod::Railpack` must plan a `railpack prepare` step before image
  build.
- R2. Railpack image build must run through Docker BuildKit with the Railpack
  frontend, not through `railpack build`.
- R3. Plain build env must be passed to `railpack prepare` as `--env KEY=VALUE`
  so detection and build planning see the same values as runtime.
- R4. Secret build env must be named during `railpack prepare` without exposing
  the real value in argv, and passed to Docker build only via private BuildKit
  secret files.
- R5. Railpack secret builds must pass an opaque keyed `secrets-hash` build arg
  derived from secret material so cache entries invalidate when secrets change
  without exposing a raw secret hash.
- R6. Docker client env must force `DOCKER_BUILDKIT=1` when the Railpack
  frontend or secrets are used.
- R7. Command debug output, failure messages, success output, and durable
  operation records must not contain plain env values, docker build arg values,
  or secret values.
- R8. Existing Dockerfile behavior must remain unchanged except for shared
  executor plumbing.
- R9. Local build success/failure semantics must stay operation-shaped:
  failed prepare or failed build marks the build operation failed and records no
  availability.

## Scope

In scope:

- `crates/ployzd/src/daemon/handlers/build/local.rs`
- Command planning for multi-step local builds.
- Railpack prepare plan/info output paths in a daemon-owned temporary build
  metadata directory.
- Railpack frontend Docker invocation using
  `BUILDKIT_SYNTAX=ghcr.io/railwayapp/railpack-frontend`.
- Unit coverage for command shape, redaction, step failure handling, and
  successful two-step Railpack execution through the handler seam.

Out of scope:

- Cloud builders or Ployz Cloud scheduling.
- Selected-machine source bundle builds.
- Persisting Railpack plan/info JSON into `ImageArtifact` provenance.
- Registry push/distribute changes.
- A real end-to-end Railpack fixture that downloads dependencies; this slice
  keeps verification at the command seam to avoid making CI depend on external
  package ecosystems.

## Existing Patterns

- `crates/ployzd/src/daemon/handlers/build/local.rs` already centralizes local
  build command planning, process execution, redaction, operation status, and
  runtime image inspection.
- `BuildCommandRunner` is the right test seam for command execution. The seam
  needs to observe multiple commands in order but should not learn business
  logic.
- `BuildInputSummary` and operation persistence already store only key names and
  secret fingerprints, never values.
- Existing build tests in `crates/ployzd/src/daemon/handlers/build/local.rs`
  cover Dockerfile command planning, Railpack CLI planning, operation failure,
  runner error redaction, and response output redaction.

## Key Decisions

- D1. Model local build execution as a `BuildCommandPlan` with ordered
  `BuildCommandStep`s instead of trying to hide `railpack prepare` behind the
  runner. This keeps the daemon's operation stage and error reporting honest.
- D2. Use Docker `buildx build` for Railpack frontend execution. Railpack docs
  show both Docker and `buildctl`; Docker keeps this path aligned with the
  Dockerfile build primitive and local image inspection.
- D3. Use `--build-arg BUILDKIT_SYNTAX=ghcr.io/railwayapp/railpack-frontend`
  and `-f <plan-file>` for the Railpack frontend. The plan file lives outside
  the app context in a temporary metadata directory, matching the documented
  frontend contract.
- D4. Keep plan/info files ephemeral in this slice. They can become provenance
  later, but persisting them now would widen the API and storage surface beyond
  the executor change.
- D5. Use a daemon-keyed, value-sensitive cache token as the `secrets-hash`
  value. The token uses length-prefixing and a daemon-owned key so argv
  observers cannot directly brute-force low-entropy secret values from a raw
  hash.

## Implementation Units

### U1. Multi-Step Local Build Plan

**Files:**
- Modify: `crates/ployzd/src/daemon/handlers/build/local.rs`

**Approach:**
- Introduce a small `BuildCommandStepKind` enum with explicit variants for
  image build and Railpack prepare.
- Add a `BuildCommandPlan` that owns ordered steps and knows how to redact
  text/output across all steps.
- Update handler execution to run each step in order, update the build operation
  stage before each step, stop on the first failed step, and use the image-build
  step for final success output rendering.

**Test Scenarios:**
- Dockerfile planning still produces one image-build step with the same args and
  env as before.
- A failing first step marks the build operation failed and does not inspect or
  record availability.
- Runner errors from any step are redacted using all plan-sensitive values.

### U2. Railpack Frontend Command Planning

**Files:**
- Modify: `crates/ployzd/src/daemon/handlers/build/local.rs`

**Approach:**
- For Railpack, generate metadata paths under a temporary directory:
  `railpack-plan.json` and `railpack-info.json`.
- Build step 1:
  `railpack prepare --plan-out <plan> --info-out <info> [--env KEY=VALUE...] .`
  where secret values use a fixed non-secret placeholder.
- Build step 2:
  `docker buildx build -t IMAGE --build-arg BUILDKIT_SYNTAX=... -f <plan>
  [--platform PLATFORM] --load [--build-arg KEY=VALUE...] [--secret
  id=KEY,src=<private-file>...] [--build-arg secrets-hash=TOKEN] .`
- Pass secret values through owner-only temporary files for the Docker build
  step. Force `DOCKER_BUILDKIT=1` for Railpack frontend builds.

**Test Scenarios:**
- Plain env appears in Railpack prepare args and is redacted in debug output.
- Secret env appears as a Railpack prepare placeholder and Docker `--secret`
  file mount; the secret value is absent from process argv, debug/failure
  output, and durable records.
- Railpack secret builds include `--build-arg secrets-hash=<token>`.
- Platform is propagated to the Docker build step.
- Railpack buildx output includes `--load` so the built image can be inspected
  from the local Docker image store.

### U3. Preserve Dockerfile Behavior

**Files:**
- Modify: `crates/ployzd/src/daemon/handlers/build/local.rs`

**Approach:**
- Route Dockerfile through the same `BuildCommandPlan` shape but keep its single
  Docker command equivalent to current behavior.
- Keep Dockerfile plain env as `--build-arg`, Dockerfile-specific build args as
  `--build-arg`, and secret env as BuildKit `--secret` mounts with
  `DOCKER_BUILDKIT=1`.

**Test Scenarios:**
- Existing Dockerfile command tests continue to pass with only structural
  updates for the plan wrapper.
- Duplicate env/build-arg key rejection and secret-like build arg rejection are
  unchanged.

### U4. Verification

**Files:**
- Modify: `crates/ployzd/src/daemon/handlers/build/local.rs`

**Approach:**
- Run focused daemon build tests first.
- Run the crate/API build test set affected by build-input surfaces.
- Run the default repo test command before PR creation.

**Test Scenarios:**
- `cargo test -p ployzd build --lib`
- `cargo test -p ployz-api -p ployz-types -p ployz-sdk -p ployzd build --lib`
- `just test`

## Risks

- Railpack frontend image references can drift. This slice uses the documented
  current image and keeps the value centralized as a constant.
- Docker `buildx` availability can differ from plain `docker build`. This slice
  surfaces that as a foreground command failure; a later capability-preflight
  slice can add explicit probes.
- Railpack docs show `railpack prepare --env KEY=value` for secrets, but the
  prose frames prepare as naming secrets. This slice intentionally uses a fixed
  placeholder value during prepare and sends the real value only through
  BuildKit secret files; if Railpack later requires value-sensitive planning,
  Ployz should keep rejecting Railpack secrets until a non-argv prepare channel
  exists.

## External References

- Railpack CLI reference: `https://railpack.com/reference/cli/`
- Railpack BuildKit frontend reference:
  `https://railpack.com/reference/frontend/`
- Docker BuildKit frontend docs:
  `https://docs.docker.com/build/buildkit/frontend/`
