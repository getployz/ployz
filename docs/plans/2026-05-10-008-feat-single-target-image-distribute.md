---
title: "feat: Single-target image distribute"
type: feat
status: completed
date: 2026-05-10
origin: docs/plans/2026-05-10-007-feat-image-receive-session-listener.md
---

# feat: Single-target image distribute

## Summary

Implement the first executable image distribution primitive: a source daemon can
send one digest-backed Docker archive to one target machine using the
target-side receiver, skip already-present receiver blobs, ask the target to
import the received artifact, and record verified image availability.

## Problem Frame

The core now has image placement contracts and target receive sessions, but
`image distribute` still returns an explicit unimplemented error. The next
slice should prove the transport loop end-to-end without adding cloud builder
behavior or hidden deploy-time reconciliation.

## Assumptions

*This plan was authored without synchronous user confirmation. The items below
are agent inferences that fill gaps in the input and should be scrutinized
during implementation and review.*

- The branch may remain stacked on the receive-session PR until that PR lands.
- Single-target distribution is enough for this slice; multi-target fanout and
  concurrency can build on the same primitive later.
- Docker archive export/import is the runtime primitive available today, so the
  transfer layer should adapt that archive into receiver blobs instead of
  inventing new runtime-specific image plumbing in this slice.

## Requirements

- R1. `ImageDistributeRequest` with exactly one target executes instead of
  returning `IMAGE_DISTRIBUTE_UNIMPLEMENTED`.
- R2. The source machine must be the local daemon, the source image must exist
  locally, and the local runtime digest must match the requested digest before
  transfer starts.
- R3. The source requests a target receive session through node RPC for remote
  targets, or through the local handler for self-target tests.
- R4. Transfer uses the target receiver's OCI registry surface and performs
  blob-level `HEAD` checks before uploading config or layer blobs.
- R5. The target imports only after the received manifest is available, then
  verifies the requested digest through the runtime backend before availability
  is recorded.
- R6. Operation records and per-target outcomes expose running, failed, and
  succeeded states with structured operator-visible errors.
- R7. Failures must not silently mark image availability as present.

## Scope Boundaries

- No `image push` orchestration yet.
- No multi-target fanout, target concurrency, retries, or partial-success
  aggregation beyond the single target result.
- No cloud builder pool, build scheduling, Dockerfile build, or Railpack build.
- No deploy preflight integration.
- No durable registry product or long-lived registry credentials.
- No dependency on Docker daemon insecure-registry configuration.

### Deferred to Follow-Up Work

- `image push IMAGE --to ...`: separate slice after distribute proves the
  transfer/import path.
- Multi-target fanout: separate slice using this target executor and per-target
  operation updates.
- Runtime-native tag/pull/push methods: separate slice if Docker archive
  adaptation becomes too limiting.

## Context & Research

### Relevant Code and Patterns

- `crates/ployzd/src/daemon/handlers/image/push.rs` owns push,
  distribute, and receive-session handlers.
- `crates/ployzd/src/daemon/handlers/image/registry.rs` owns the session-gated
  receiver, blob CAS, and manifest storage.
- `crates/ployzd/src/daemon/handlers/image/operations.rs` owns durable image
  operation records.
- `crates/ployzd/src/daemon/handlers/image/inspect.rs` shows how runtime
  verification records image availability truth.
- `crates/ployz-runtime-api/src/image.rs` exposes `export_image_archive`,
  `import_image_archive`, and `verify_image_digest`.
- `crates/ployz-nats/src/coord/rpc.rs` provides node RPC for target receive
  sessions.

### Institutional Learnings

- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`
  reinforces the intent/status/observation split. Receiver CAS blobs are
  transfer cache; verified runtime image presence is availability truth.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`
  reinforces decision-time probes and explicit preconditions before mutation.

## Key Technical Decisions

- **Adapt Docker archive into receiver blobs.** The source runtime already
  exports an image archive. Parsing that archive lets the transfer perform
  per-blob `HEAD` checks and skip existing config/layer blobs before uploading
  the generated manifest.
- **Import from receiver CAS, not by Docker pull.** Docker pull would require
  target daemon insecure-registry configuration for non-local endpoints. The
  target can instead reconstruct a Docker-loadable archive from its own
  receiver files and call the existing runtime import primitive.
- **Verify before recording availability.** The operation records success only
  after the target runtime verifies the requested digest; upload success alone
  is not image presence.
- **Keep single-target hard-gated.** Reject zero or multiple targets in this
  slice so callers cannot mistake the MVP for fanout semantics.

## Open Questions

### Deferred to Implementation

- Exact archive parser shape: implementation should inspect the Docker archive
  format emitted by the current backend and keep parsing limited to config,
  layer paths, and repo tag metadata needed for import.
- Exact import finalization request name: implementation may add a narrow
  internal daemon request if it keeps target import explicit and testable.

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for
> review, not implementation specification.*

`image distribute digest D --from source --to target` should:

1. create a durable distribute operation record,
2. verify `source == local machine`,
3. verify local runtime contains `D`,
4. export the source image archive and derive config/layer blob digests,
5. request a target receive session,
6. `HEAD` each blob at the target receiver, uploading only missing blobs,
7. upload the generated manifest under a deterministic operation reference,
8. ask the target daemon to import the received artifact,
9. verify target runtime digest `D`,
10. upsert a `Present` availability record and mark the operation succeeded.

## Implementation Units

### U1. Archive-to-Receiver Transfer Adapter

**Goal:** Convert a Docker export archive into receiver blobs and a manifest
while preserving layer skip behavior.

**Requirements:** R2, R4

**Dependencies:** None

**Files:**

- Create: `crates/ployzd/src/daemon/handlers/image/archive.rs`
- Modify: `crates/ployzd/src/daemon/handlers/image.rs`
- Modify: `crates/ployzd/Cargo.toml`
- Modify: `Cargo.toml`
- Test: `crates/ployzd/src/daemon/handlers/image/archive.rs`

**Approach:**

- Spool the async runtime archive to a temp file under `data_dir`.
- Parse only Docker archive members needed for transfer: `manifest.json`,
  config JSON, and ordered layer files.
- Compute `sha256:` digests for config/layer blobs from file contents.
- Generate a Docker/OCI manifest body that references those blobs.
- Provide an async upload client that adds receive-session headers and performs
  `HEAD` before monolithic blob upload.

**Test scenarios:**

- Happy path: archive parser extracts config, ordered layers, repo tag, and
  digests from a minimal Docker archive.
- Happy path: upload adapter skips a blob when receiver `HEAD` returns 200.
- Error path: malformed `manifest.json` fails before uploading any blob.
- Error path: digest mismatch from receiver upload returns a structured error.

**Verification:**

- `cargo test -p ployzd --no-default-features image_archive`

### U2. Target Received-Image Import Request

**Goal:** Add a target-side internal request that imports an already-uploaded
received image artifact and verifies runtime digest before mutating availability.

**Requirements:** R5, R7

**Dependencies:** U1

**Files:**

- Modify: `crates/ployz-api/src/image.rs`
- Modify: `crates/ployz-api/src/request.rs`
- Modify: `crates/ployz-api/src/response.rs`
- Modify: `crates/ployz-nats/src/coord/rpc.rs`
- Modify: `crates/ployzd/src/daemon/handlers/image/push.rs`
- Modify: `crates/ployzd/src/daemon/handlers/mod.rs`
- Modify: `crates/ployzd/src/metrics.rs`
- Test: `crates/ployz-api/src/request.rs`
- Test: `crates/ployz-nats/src/coord/rpc.rs`
- Test: `crates/ployzd/src/daemon/handlers/image/push.rs`

**Approach:**

- Add an internal request carrying operation id, source machine, repository,
  reference, expected digest, and optional platform.
- Reconstruct a Docker-loadable archive from receiver CAS blobs and manifest
  data.
- Import through `RuntimeImageBackend::import_image_archive`.
- Verify `expected_digest` through `verify_image_digest`.
- Upsert `ImageAvailabilityRecord::Present` only after verification.

**Test scenarios:**

- Happy path: received manifest plus blobs imports and records present
  availability for the target.
- Error path: missing blob fails without availability mutation.
- Error path: runtime import failure records no availability.
- Error path: digest verification failure records no availability.
- Wire shape: internal request serializes operation id, repository, reference,
  and expected digest.

**Verification:**

- `cargo test -p ployz-api --no-default-features image_received`
- `cargo test -p ployz-nats --no-default-features image_received`
- `cargo test -p ployzd --no-default-features image_received`

### U3. Single-Target Image Distribute Handler

**Goal:** Execute one target transfer end-to-end and expose a completed
`ImageDistributePayload`.

**Requirements:** R1, R2, R3, R4, R5, R6, R7

**Dependencies:** U1, U2

**Files:**

- Modify: `crates/ployzd/src/daemon/handlers/image/push.rs`
- Modify: `crates/ployzd/src/daemon/handlers/image/operations.rs`
- Test: `crates/ployzd/src/daemon/handlers/image/push.rs`

**Approach:**

- Reject zero or multiple target machines with a specific structured error.
- Reject non-local source machines for now.
- Begin a distribute operation record before transfer starts.
- Resolve target receive session locally for self-target or over node RPC for a
  remote target.
- Upload missing blobs and manifest to the target receiver.
- Ask the target to import/verify the received image.
- Update operation target outcome and return `ImageDistributePayload`.

**Test scenarios:**

- Happy path: single-target distribute returns `Present`, includes operation id,
  digest, source, and availability record.
- Happy path: target with existing receiver blobs is reported present after
  skipping uploads and importing.
- Error path: zero targets is rejected before operation side effects.
- Error path: multiple targets is rejected with a multi-target-unimplemented
  code.
- Error path: non-local source is rejected.
- Error path: receive-session RPC failure updates operation and target failure.

**Verification:**

- `cargo test -p ployzd --no-default-features image_distribute`

## System-Wide Impact

- **Interaction graph:** user/API command -> source daemon shared handler ->
  target daemon receive session -> target receiver HTTP endpoints -> target
  internal import RPC -> runtime backend -> availability store.
- **Error propagation:** transport, parsing, RPC, import, and verification
  errors become handler response errors plus operation-record target outcomes.
- **State lifecycle risks:** uploaded receiver blobs are cache and may outlive a
  failed operation; availability mutates only after runtime verification.
- **API surface parity:** CLI and SDK already have `image distribute`; this
  slice fills daemon behavior without adding new user CLI flags.
- **Integration coverage:** unit tests prove parser, receiver skip, internal
  target import, and handler orchestration. Full Docker e2e remains a follow-up.
- **Unchanged invariants:** `image push` remains explicitly unimplemented;
  deploy planning does not invoke image transfer implicitly.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Docker archive format assumptions are too broad | Parse only the stable fields required by Docker save/load and cover with minimal archive fixtures |
| Receiver CAS upload succeeds but target import fails | Preserve operation failure and avoid availability mutation |
| Remote Docker pull/insecure registry issues | Do not use Docker pull; import from target-local receiver CAS through runtime archive import |
| Stacked branch churn from PR #164 | Keep changes focused and base the new PR on `codex/image-runtime-transport` while #164 is open |

## Verification

- `cargo fmt --check`
- `cargo test -p ployz-api --no-default-features image_received`
- `cargo test -p ployz-nats --no-default-features image_received`
- `cargo test -p ployzd --no-default-features image_archive`
- `cargo test -p ployzd --no-default-features image_received`
- `cargo test -p ployzd --no-default-features image_distribute`
- `just test-all`
