---
title: MVP Data-Plane Parity Slice 1 Runtime Backend Boundary
status: active
created: 2026-05-20
type: feature
parent_plan: docs/plans/2026-05-20-002-feat-mvp-data-plane-parity.md
---

# MVP Data-Plane Parity Slice 1 Runtime Backend Boundary

## Problem Frame

The MVP deploy path currently proves workload behavior with
`mvp_runtime::ProcessRuntime`, which starts a child HTTP process and records
loopback endpoints. That is useful as a fixture, but it is now blocking real
data-plane parity. Slice 1 introduces the runtime abstraction that lets
`mvp-node` drive either the existing process fixture or a real Docker backend
without changing deploy coordination.

This slice is intentionally runtime-only. It should not solve WireGuard,
container overlay routing, service DNS, Pingora, or ACME. It prepares the
runtime endpoint model those later slices will consume.

If Docker implementation details make this slice too broad, split it before
coding. The minimum quality bar is a clear runtime contract first, then a
Docker backend with its own small modules. Do not hide container spec building,
Docker client calls, label parsing, adoption, readiness, and node-agent RPC
inside one large file.

## Requirements From Parent Plan

- Covers R3: workloads run through a real container runtime backend, not
  `ProcessRuntime`.
- Prepares R4/R5 by returning typed runtime endpoints and container identity
  instead of embedding loopback-only assumptions in deploy code.
- Supports R13 by adopting already-running containers after daemon restart.
- Keeps backend-specific Docker code below the runtime boundary.

## Current Code Shape

- `MVP/runtime/src/process.rs` owns concrete prepare/start/drain/stop/list
  methods and local metadata persistence.
- `MVP/runtime/src/model.rs` uses `ProcessInstanceSpec` and `ProcessInstance`,
  which makes the runtime contract process-shaped.
- `MVP/node/src/node_agent.rs` stores `Option<ProcessRuntime>` directly inside
  `NodeAgentRuntime`, and each handler calls process methods directly.
- `MVP/node/src/deploy.rs` can inject a `ProcessRuntime` for tests but cannot
  inject a different backend.
- `crates/ployz-runtime-backends/src/deploy/local.rs` and
  `crates/ployz-runtime-backends/src/runtime/*` already contain Docker runtime
  patterns, label constants, drift comparison, and image/container handling to
  reuse conceptually without importing the old crate upward into command logic.
- The user preference for complex substrate work is to port proven mechanics
  from the pre-existing codebase where they fit. For this slice, that means
  copying/adapting the useful Docker label/spec/engine/readiness ideas into
  `mvp-runtime` behind `RuntimeBackend`, not reusing old orchestration types or
  inventing a parallel runtime model from scratch.

## Design Decisions

### Runtime trait lives in `mvp-runtime`

`mvp-runtime` should define the trait and shared DTOs because `mvp-node` needs
the abstraction and both process and Docker implementations belong below the
node-agent/deploy coordination layer.

### Keep process runtime as a fixture backend

Do not delete `ProcessRuntime`. Rename or wrap it as the fixture/local backend
only if that clarifies the API. Existing unit tests should continue to use it
where they are not testing Docker.

### Docker backend returns runtime-owned endpoints

The backend should return a structured endpoint with instance id, node id,
container id/name, network attachment data, and best available address. Slice 1
may still expose a host-published or bridge address for focused tests, but the
shape must not hard-code loopback as the product address.

### Node-agent state follows backend observations

The node agent should populate prepared/running/draining/stopped views from
runtime `list`/`adopt` results. It should not maintain a parallel truth that
can drift from the runtime.

### Docker availability is a runtime readiness failure

If Docker is not available in a non-Docker test environment, focused unit tests
should still pass through the process fixture. Docker integration tests should
be Linux/Docker-gated and report a concrete skip/blocker rather than silently
falling back to process runtime.

### Module boundaries are part of the deliverable

The Docker backend should be split by concept from the start:

- runtime contract and DTOs,
- process fixture backend,
- Docker labels and identity,
- Docker spec/container lifecycle,
- Docker adoption/listing,
- readiness/probing.

If any production module grows toward 1,000 LOC or starts owning multiple
items from that list, stop and split before adding behavior.

## Implementation Units

### Unit 1: Runtime Contract DTOs

Files:

- `MVP/runtime/src/model.rs`
- `MVP/runtime/src/error.rs`
- `MVP/runtime/src/lib.rs`

Work:

- Introduce runtime-neutral names:
  - `RuntimeInstanceSpec`
  - `RuntimeInstance`
  - `RuntimeEndpoint`
  - `RuntimeInstanceState`
- Keep compatibility aliases or narrow migration helpers only inside this
  slice if needed to avoid a noisy mechanical change.
- Add fields needed by Docker adoption: backend id, backend name, service,
  revision, endpoint address, and state.
- Keep `InstanceId`, `ServiceName`, `RevisionId`, and `BackendEndpoint`
  conversions typed.

Tests:

- DTO serialization round-trips for persisted runtime metadata.
- `RuntimeEndpoint` converts to `mvp_projection::BackendEndpoint` without
  string parsing in callers.
- State conversion preserves prepared/running/draining/stopped distinctions.

### Unit 2: RuntimeBackend Trait And Process Implementation

Files:

- `MVP/runtime/src/lib.rs`
- `MVP/runtime/src/process.rs`
- `MVP/node/src/node_agent.rs`
- `MVP/node/src/deploy.rs`

Work:

- Add a `RuntimeBackend` trait with prepare/start/drain/stop/list/adopt
  operations.
- Implement the trait for `ProcessRuntime`.
- Change `NodeAgentRuntime` to hold `Arc<dyn RuntimeBackend>` or an explicit
  runtime enum instead of `Option<ProcessRuntime>`.
- Keep in-memory/no-runtime test mode explicit; do not use `None` to mean both
  "fixture" and "runtime unavailable".
- Update deploy injection helpers to accept the trait-backed runtime.

Tests:

- Existing node-agent in-memory tests still pass.
- A process-backed node-agent test proves prepare/start returns a backend
  endpoint and drain/stop map from endpoint back to instance id.
- Restart/adoption test proves a new node-agent instance reads existing
  process runtime metadata into running state.

### Unit 3: Docker Runtime Backend

Files:

- `MVP/runtime/Cargo.toml`
- `MVP/runtime/src/lib.rs`
- `MVP/runtime/src/docker.rs`
- `MVP/runtime/src/error.rs`
- `MVP/runtime/src/model.rs`

Reference files:

- `crates/ployz-runtime-backends/src/runtime/labels.rs`
- `crates/ployz-runtime-backends/src/runtime/spec.rs`
- `crates/ployz-runtime-backends/src/runtime/engine.rs`
- `crates/ployz-runtime-backends/src/deploy/local.rs`

Work:

- Add a Docker-backed `RuntimeBackend` behind a feature or Linux-only module.
- Split Docker implementation into small modules if needed; `docker.rs` should
  be a facade, not a catch-all.
- Start containers with stable labels:
  - island
  - node id
  - instance id
  - service
  - revision
  - managed-by marker
- Use deterministic container names so adoption can find existing instances.
- Implement prepare as metadata/spec persistence, start as ensure-container,
  drain as state/label transition or no-new-traffic marker, stop as bounded
  container stop/remove, and list/adopt from Docker inspect/list.
- Return typed runtime endpoints from Docker inspect/network settings.

Tests:

- Unit tests for Docker label construction and parsing that do not require a
  Docker daemon.
- Unit tests for container name generation and instance identity extraction.
- Docker-gated integration test that starts a tiny HTTP container, lists it,
  reconstructs the backend, and stops it.

### Unit 4: Node-Agent Docker Selection And Product Deploy Hook

Files:

- `MVP/node/src/node_agent.rs`
- `MVP/node/src/deploy.rs`
- `MVP/node/src/main.rs`
- `MVP/node/src/config.rs`
- `MVP/e2e/src/three_server_harness.rs`

Work:

- Add a runtime selection path for product runs: process fixture for existing
  local tests, Docker for real data-plane mode.
- Make runtime selection visible in status/readiness output where the current
  command surfaces already report runtime behavior.
- Keep existing three-server product smoke on process runtime until later
  slices upgrade the full parity scenario, but add a focused Docker deploy
  hook or gated scenario.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-runtime`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node node_agent`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node product_deploy`
- Docker-gated deploy smoke proving node-agent can start a container backend
  through the same deploy participant RPC path.

## Acceptance Checklist

- `mvp-node` no longer stores or calls `ProcessRuntime` directly except in
  process-fixture construction.
- Deploy coordination consumes runtime endpoints through the runtime contract.
- Docker backend starts, lists/adopts, drains, and stops containers with stable
  labels.
- Process fixture behavior remains available for fast tests.
- Docker unavailability is reported as a runtime backend failure in Docker mode,
  not silently replaced by the process fixture.
- The slice is committed and pushed before Slice 2 starts.

## Verification Commands

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-runtime`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node node_agent`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node product_deploy`
- Docker-gated command to be added by this slice once the backend exists.

## Explicit Deferrals

- Overlay attachment and service DNS move to Slice 3.
- WireGuard interface application moves to Slice 2.
- Pingora/HTTPS integration moves to Slice 4.
- Pebble ACME issuance moves to Slice 5.
