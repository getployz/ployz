---
title: MVP Data-Plane Parity Slice 3 Container Overlay Networking And Service DNS
status: active
created: 2026-05-20
type: feature
parent_plan: docs/plans/2026-05-20-002-feat-mvp-data-plane-parity.md
---

# MVP Data-Plane Parity Slice 3 Container Overlay Networking And Service DNS

## Problem Frame

Slices 1 and 2 gave the MVP real runtime and host WireGuard boundaries. Slice 3
connects those boundaries: deployed containers need stable overlay-facing
addresses, host forwarding to the WireGuard interface, and container-facing
DNS that resolves service names to projected service backends across nodes.

This slice must not turn runtime, mesh, DNS, and eBPF into one god module. The
missing concepts are:

- per-node container subnet ownership,
- Docker bridge lifecycle and container attachment,
- overlay forwarding rules from bridge to WireGuard,
- service DNS projection for containers,
- runtime endpoint addresses that represent overlay reachability, not loopback.

## Requirements From Parent Plan

- Covers R4: service backends are reachable over the overlay, including
  gateway-to-backend and container-to-container paths across different nodes.
- Covers R5: service DNS resolves deployed services from inside containers
  across the overlay.
- Prepares R9/R12 by making `web`, `api`, `echo`, and one-shot `client`
  containers reachable across three equal nodes.
- Keeps Docker, eBPF, iptables, and DNS substrate details below runtime/mesh/
  serving boundaries.

## Existing Code To Port From

Preferred source material:

- `crates/ployz-runtime-backends/src/network/docker_bridge.rs`
  - Docker bridge create/inspect/connect/disconnect/remove.
  - deterministic network naming and bridge interface resolution.
  - Docker IPAM config and static container IP assignment.
  - iptables exemptions for overlay-to-bridge traffic.
- `crates/ployz-orchestrator/src/mesh/container_network.rs`
  - `ContainerNetworkBackend` trait shape.
  - Small facade over backend lifecycle/attachment operations.
- `crates/ployz-bpfctl/src/linux.rs`, `crates/ebpf-common`, and `ebpf/`
  - TC attach/detach.
  - pinned route and observe maps.
  - route add/delete by subnet to WireGuard ifindex.
  - IPv4 subnet and IPv6 ULA forwarding from Docker bridge to WG.
- `MVP/serving/src/dns_server.rs`
  - hickory-proto DNS message handling.
  - current DNS state boundary over `WireServingState`.

Port mechanics behind MVP owner crates. Do not import old orchestrator/runtime
types upward or conflate service DNS with mutable service registry state.

## Design Decisions

### Container Networking Belongs Below Runtime/Mesh

`mvp-runtime` can ask for a network attachment and return an endpoint address,
but the network concept should be explicit rather than buried inside Docker
container lifecycle code. Add a small container-network boundary instead of
making `DockerRuntime` own bridge creation, route programming, service DNS,
and container start all at once.

### Subnet Ownership Is Deterministic

Each node gets a deterministic IPv4 container subnet derived from island/node
identity. The subnet is stored/visible as node-local data-plane state and used
for:

- Docker bridge IPAM,
- local container static addresses,
- WireGuard peer allowed IPs in later peer plans,
- eBPF/route map entries for remote container subnets.

Avoid a mutable IPAM lease table for this slice.

### eBPF Is A Forwarding Adapter, Not Service Logic

The eBPF pieces should know route prefixes and interface indices only. They
must not know service names, revisions, deployments, or projection facts.
Service-to-subnet decisions stay in Rust domain code.

### DNS Is Container-Facing Projection

Service DNS answers come from projected service/backend state plus runtime
endpoint metadata. DNS must not write service truth or observe liveness into
durable facts. It should serve current projection with clear stale/empty
behavior.

### Runtime Endpoints Must Stop Being Loopback Product Addresses

For Docker mode, `RuntimeInstance.address` should be the container overlay
address and service port. Loopback remains valid only for the process fixture.
Gateway and service DNS should route using the returned endpoint without
stringly special cases.

## Implementation Units

### Unit 1: Container Network Model And Deterministic Subnets

Status: complete in checkpoint pending push.

Files:

- `MVP/mesh/src/container.rs`
- `MVP/node/src/state.rs`

Work:

- [x] Add typed `ContainerSubnet`, `ContainerIp`, and deterministic derivation from
  island/node id.
- [x] Store node-local container subnet in state or derive it from stable state in
  one place.
- [x] Add conversions to Docker IPAM strings and peer allowed CIDRs.

Tests:

- [x] deterministic subnet derivation is stable and node-scoped,
- [x] subnets do not overlap for representative node sets,
- [x] typed subnet/IP serialization round trips.

Evidence:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-mesh`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-mesh -p mvp-node state`

### Unit 2: Docker Bridge Backend

Status: complete in checkpoint pending push.

Files:

- `MVP/runtime/src/network/mod.rs`
- `MVP/runtime/src/network/docker_bridge.rs`
- `MVP/runtime/Cargo.toml`

Work:

- [x] Port Docker bridge lifecycle from `crates/ployz-runtime-backends`.
- [x] Keep bridge creation/inspection/connect/disconnect in a network backend,
  not in `DockerRuntime` lifecycle.
- [x] Resolve bridge interface name for eBPF attachment.
- [x] Return gateway/container IPs through typed structs.

Tests:

- [x] unit tests for network names, IPAM config, and bridge interface naming,
- [x] Docker-gated integration test creates/removes a bridge network,
- [x] Docker-gated test connects a busybox container at a static IP.

Evidence:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-runtime --features docker --test docker_bridge_network`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-runtime --features docker`

### Unit 3: Overlay Forwarding Adapter

Status: complete except privileged smoke execution in this non-root/default
environment. The gated smoke is implemented and skipped unless
`MVP_LINUX_FORWARDING_SMOKE=1`.

Files:

- `MVP/mesh/src/forwarding.rs`
- `MVP/mesh/tests/linux_forwarding_backend.rs`

Work:

- [x] Port the iptables exemptions and eBPF route-map mechanics behind a
  forwarding trait.
- [x] Attach TC classifiers to the Docker bridge only in Linux data-plane mode.
- [x] Program remote container subnets to the WG ifindex.
- [x] Keep observation toggles separate from forwarding correctness.

Tests:

- [x] non-privileged unit tests for route-key encoding and command construction,
- [x] Linux-gated smoke attaches/detaches on a throwaway bridge when enabled,
- [x] no service/domain types in forwarding adapter APIs.

Evidence:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-mesh`
- Privileged smoke command for a root Linux runner with `ployz-bpfctl`
  available:
  `MVP_LINUX_FORWARDING_SMOKE=1 cargo test --manifest-path MVP/Cargo.toml -p mvp-mesh --test linux_forwarding_backend -- --nocapture`

### Unit 4: Runtime Network Attachment

Status: complete in checkpoint pending push.

Files:

- `MVP/runtime/src/docker/backend.rs`
- `MVP/runtime/src/docker/spec.rs`
- `MVP/runtime/src/model.rs`
- `MVP/node/src/deploy.rs`

Work:

- [x] Let Docker runtime accept a container network backend/config.
- [x] Start containers on the bridge with static service IPs.
- [x] Return overlay endpoint addresses from Docker inspect/attachment metadata.
- [x] Preserve process fixture behavior.

Tests:

- [x] Docker runtime starts/list/adopts with overlay endpoint,
- [x] changed revision preserves service IP policy or intentionally moves to a new
  deterministic IP,
- [x] node-agent Docker RPC returns overlay endpoint.

Evidence:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-runtime --features docker`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node --features docker-runtime docker_node_agent`

### Unit 5: Container-Facing Service DNS

Status: complete except privileged smoke execution in this non-root/default
environment. Service-DNS answers are implemented as a projection-derived read
view, Docker deployments default container DNS to the node bridge gateway, and
the Docker one-shot client smoke is implemented behind
`MVP_DOCKER_SERVICE_DNS_SMOKE=1`.

Files:

- `MVP/serving/src/dns_server.rs`
- `MVP/serving/src/model.rs`
- `MVP/serving/src/actor.rs`
- `MVP/serving/src/wire.rs`
- `MVP/runtime/src/docker/spec.rs`
- `MVP/runtime/src/docker/backend.rs`
- `MVP/node/src/main.rs`
- `MVP/node/tests/container_service_dns.rs`

Work:

- [x] Add a service-DNS view for names like `echo.<test-domain>` or
  `echo.service.<domain>` based on projected service backends.
- [x] Configure Docker containers to use the node-local DNS listener.
- [x] Return A records for overlay container addresses.
- [x] Keep existing public route DNS semantics intact.

Tests:

- [x] DNS unit tests for service names and unknown services,
- [x] integration test runs DNS server and resolves a projected service,
- [x] Docker-gated one-shot client resolves and curls another container by service
  DNS on the same host before cross-machine smoke.

Evidence:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-serving`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-runtime --features docker`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node --features docker-runtime --test container_service_dns -- --nocapture`
- Privileged smoke command for a root Linux runner:
  `MVP_DOCKER_SERVICE_DNS_SMOKE=1 cargo test --manifest-path MVP/Cargo.toml -p mvp-node --features docker-runtime --test container_service_dns -- --nocapture`

### Unit 6: Cross-Node Overlay Smoke Harness

Files:

- `MVP/e2e/src/...`

Work:

- Add a Linux-gated two-node smoke before the full parity smoke:
  - two nodes with Docker bridge + WG + service DNS,
  - service on node-b,
  - client on node-a resolves and curls over overlay.
- Record clear skip/blocker when not root or Docker/WG/eBPF prerequisites are
  missing.

Tests:

- gated smoke command documented in this plan,
- cleanup proves bridge, eBPF attach, and containers are removed.

## Acceptance Checklist

- Container subnet ownership is typed and deterministic.
- Docker bridge lifecycle is behind a runtime/network backend.
- Docker runtime returns overlay endpoints in Docker mode.
- eBPF/iptables forwarding is behind a mesh forwarding adapter and has no
  service/deploy knowledge.
- Service DNS resolves projected services to overlay endpoints from inside a
  container.
- Existing process fixture and non-privileged tests remain green.
- Gated Linux/Docker smoke proves container-to-container traffic over the
  overlay or records a concrete host prerequisite blocker.

## Verification Commands

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-runtime --features docker`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-mesh --features linux-wireguard`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-serving`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node --features docker-runtime,linux-wireguard`
- Gated smoke command to be finalized once Unit 6 lands.

## Explicit Deferrals

- Pingora gateway remains Slice 4.
- Pebble ACME remains Slice 5.
- Full three-node HTTPS parity smoke remains the final smoke slice.
- ZFS and BSD/Darwin remain out of scope.
