---
title: "feat: Image receive session listener"
type: feat
status: active
date: 2026-05-10
origin: docs/plans/2026-05-10-006-feat-layer-delta-image-placement-slice.md
---

# feat: Image receive session listener

## Problem Frame

The layer-delta image placement foundation added an ephemeral OCI receiver, but
left it intentionally unmounted. The next slice should make that receiver a
daemon-owned runtime primitive so later `image push` and `image distribute`
orchestration can ask a target machine for a short-lived receive session and
push OCI layers to an overlay-reachable endpoint.

This remains core open-source plumbing. It must not introduce a durable registry
product, a cloud builder concept, or hidden deploy-time image distribution.
The cluster still changes only through explicit commands.

## Scope Boundaries

- No Docker `tag`, `push`, `pull`, or `untag` implementation yet.
- No source-side localhost proxy yet.
- No full `image push` or `image distribute` orchestration yet.
- No deploy preflight behavior changes.
- No managed registry surface or long-lived registry credentials.
- No cloud builder behavior.

## Requirements

- R1. Mount the existing `ImageRegistry` router behind a daemon-owned listener
  that starts and stops with the active mesh.
- R2. Bind the image receiver using the same runtime-profile posture as the ZFS
  transfer listener: localhost for Docker runtime, overlay IP for host runtime,
  and no listener for memory tests.
- R3. Add a target-side daemon request that opens an image receive session for an
  operation id and source machine id, returning endpoint URL, session token, and
  required Ployz headers.
- R4. Route the receive-session request over the existing NATS node RPC subject
  model so a coordinator can later ask a target machine to prepare for incoming
  image layers.
- R5. Keep receive sessions short-lived and explicit; session creation should
  not record image availability or claim transfer success.
- R6. Remove the registry module-wide dead-code allowance by wiring the receiver
  into production code.
- R7. Cover listener lifecycle planning, request wire shape, node RPC subject,
  receive-session handler, and registry route reachability with tests.

## Key Decisions

- **Reuse the mesh startup handle pattern.** `ActiveMesh` already owns
  shutdown-capable runtime handles for NATS control, ZFS transfer, gateway, and
  DNS. The image receiver should be another explicit handle rather than a loose
  background task.
- **Expose sessions through daemon RPC, not registry auth discovery.** The OCI
  receiver stays a transport detail. Callers obtain Ployz headers through a
  typed daemon request, then Docker/source proxy plumbing can use those headers
  later.
- **Use a separate image receiver port.** Sharing the ZFS transfer port would
  couple unrelated protocols. This slice can derive a deterministic port from
  `zfs_transfer_port + 1` until a dedicated config knob is justified.
- **Keep availability truth separate.** A session and uploaded CAS blobs are
  transfer cache only. Image availability records still change only when a later
  orchestration step verifies the image is present in the runtime.

## Existing Patterns To Follow

- Mesh startup handle ownership in `crates/ployzd/src/daemon/setup.rs`.
- Active handle shutdown in `crates/ployzd/src/app.rs` and
  `crates/ployzd/src/daemon/handlers/mesh/lifecycle.rs`.
- Runtime-profile bind selection in `crates/ployzd/src/daemon/runtime.rs`.
- ZFS transfer listener shape in
  `crates/ployzd/src/daemon/handlers/volume/transfer_listener.rs`.
- Node RPC subject helpers in `crates/ployz-nats/src/coord/rpc.rs`.
- Request and response wire-shape tests in `crates/ployz-api/src/request.rs`.

## Implementation Units

### U1. API And RPC Session Surface

**Goal:** Add a typed target-side receive-session request and response payload
without changing user-facing `image push` behavior yet.

**Files:**

- Modify: `crates/ployz-api/src/image.rs`
- Modify: `crates/ployz-api/src/request.rs`
- Modify: `crates/ployz-api/src/response.rs`
- Modify: `crates/ployz-nats/src/coord/rpc.rs`
- Test: `crates/ployz-api/src/request.rs`
- Test: `crates/ployz-nats/src/coord/rpc.rs`

**Approach:**

- Add `ImageReceiveSessionRequest` with `operation_id`, `source_machine`, and
  optional `repository`.
- Add `ImageReceiveSessionPayload` with the target machine, endpoint URL,
  token, expiry, and the three required registry header values.
- Add `DaemonRequest::ImageReceiveSession` and
  `DaemonPayload::ImageReceiveSession`.
- Add `NodeCommandSubject::image_receive_session`.
- Keep this request internal/target-side; do not add a CLI command.

**Test Scenarios:**

- Request wire shape includes operation id, source machine, and repository.
- Payload wire shape includes endpoint and required header map.
- NATS subject is authority-scoped as
  `image.receive_session`.

### U2. Daemon-Owned Image Receiver Listener

**Goal:** Start and stop the registry receiver with mesh lifecycle, and make its
bind address discoverable by receive-session handlers.

**Files:**

- Modify: `crates/ployzd/src/daemon/mod.rs`
- Modify: `crates/ployzd/src/daemon/runtime.rs`
- Modify: `crates/ployzd/src/daemon/setup.rs`
- Modify: `crates/ployzd/src/app.rs`
- Modify: `crates/ployzd/src/daemon/handlers/mesh/lifecycle.rs`
- Modify: `crates/ployzd/src/daemon/handlers/image/registry.rs`
- Test: `crates/ployzd/src/daemon/setup.rs`
- Test: `crates/ployzd/src/daemon/handlers/image/registry.rs`

**Approach:**

- Add an `image_receiver` runtime handle to `ActiveMesh`.
- Add a bind-address helper that uses `zfs_transfer_port + 1`.
- Introduce an `ImageRegistryListenerHandle` that binds an Axum server to the
  planned address and shuts down through `RuntimeHandle`.
- Store `ImageRegistry` on daemon state so handlers can register sessions
  against the same in-memory registry the listener serves.
- Start the listener after NATS control and before edge runtimes; on failure,
  roll back already-started handles like other control-plane listeners.
- Remove the module-level `dead_code` allowance from `registry.rs`.

**Test Scenarios:**

- Docker runtime plan binds image receiver to localhost on `zfs_transfer_port +
  1`.
- Host runtime plan binds image receiver to overlay IP on `zfs_transfer_port +
  1`.
- Memory test runtime uses a noop image receiver handle.
- Listener router responds to `/v2/` through the mounted server.

### U3. Receive-Session Handler

**Goal:** Let a target daemon create a short-lived session for a source machine
and return the exact endpoint/headers required for an upload.

**Files:**

- Modify: `crates/ployzd/src/daemon/handlers/image.rs`
- Modify: `crates/ployzd/src/daemon/handlers/image/push.rs`
- Modify: `crates/ployzd/src/daemon/handlers/mod.rs`
- Modify: `crates/ployzd/src/metrics.rs`
- Test: `crates/ployzd/src/daemon/handlers/image/push.rs`
- Test: `crates/ployzd/src/daemon/handlers/mod.rs`

**Approach:**

- Route `ImageReceiveSession` through the shared lane.
- Require an active mesh and running image receiver before creating a session.
- Register the session on the daemon's `ImageRegistry`.
- Return endpoint URL using the active image receiver bind address and
  repository path hint.
- Do not create image operation records and do not mutate availability records.

**Test Scenarios:**

- Handler without active mesh fails with a structured image receiver error.
- Handler with active mesh returns token, expiry, endpoint, and header values.
- The returned token authorizes a registry upload through the same registry.
- Request routing classifies receive-session as shared lane.

## Verification

- `cargo fmt --check`
- `cargo test -p ployz-api --no-default-features image_receive`
- `cargo test -p ployz-nats --no-default-features image_receive`
- `cargo test -p ployzd --no-default-features image_receiver`
- `cargo test -p ployzd --no-default-features image_receive`
- `just test-all`

## Follow-Up Slices

- Add Docker runtime image tag/push/pull/untag methods.
- Add source-side localhost proxy that injects session headers for Docker push.
- Implement `image push` orchestration with per-target outcomes.
- Implement `image distribute` from a cluster source machine.
- Add Docker interop/e2e coverage for real layer skip behavior.
