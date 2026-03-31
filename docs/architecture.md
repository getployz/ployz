# Architecture

## The Core Idea

ployzd is disposable control plane. It can crash, upgrade, restart — and nothing in the
data plane notices. WireGuard tunnels stay up, Corrosion keeps replicating, the gateway
keeps proxying, DNS keeps resolving, and workload containers keep running. On startup
the daemon attaches to whatever is already running and only recreates things whose
configuration has drifted.

This is the north star. Every design decision flows from it.

## Runtime Model

The public runtime surface is split across two axes:

| Runtime target | Service mode | Meaning |
|----------------|--------------|---------|
| Docker | User | Docker-backed mesh/store/sidecars with loopback control-plane binding |
| Host | User | Host-backed mesh/store, child-process sidecars, overlay control-plane binding |
| Host | System | Host-backed mesh/store, system-managed sidecars, overlay control-plane binding |

`Memory` is test-only. It is not an operator-facing runtime and does not shape the daemon's
public API.

## Docker Runtime (macOS)

```
macOS Host                           Docker Desktop VM (Linux)
┌─────────────────┐                  ┌───────────────────────────────────┐
│ ployzd daemon   │                  │ ployz-networking container        │
│                 │    WG bridge     │   wg0 interface (overlay network) │
│  OverlayBridge ─┼──UDP─over─TCP──►│   fd00::x overlay IPs             │
│  (userspace WG) │    127.0.0.1    │                                   │
│                 │                  │ ployz-corrosion (container:plz-nw)│
│  Transport::    │    bridge fwd   │   Corrosion API on overlay IP     │
│  Bridge ────────┼──127.0.0.1:8080─┼──►[fd00::x]:8080                 │
│                 │                  │                                   │
│                 │                  │ ployz-gateway (container:plz-nw)  │
│                 │                  │   HTTP proxy on overlay IP        │
│                 │                  │                                   │
│                 │                  │ ployz-dns (container:plz-nw)      │
│                 │                  │   DNS on [overlay]:53 + bridge:53 │
│                 │                  │                                   │
│                 │                  │ workload containers               │
│                 │                  │   Docker bridge network           │
│                 │                  │   DNS -> ployz-networking (.2)    │
└─────────────────┘                  └───────────────────────────────────┘
```

The daemon runs on the macOS host. Everything else runs inside Docker Desktop's Linux VM.
Corrosion, Gateway, and DNS need to **bind** on the node's overlay IPv6 address so other
mesh nodes can reach them directly. They share `ployz-networking`'s network namespace
(`network_mode: container:ployz-networking`) to get access to the `wg0` interface.

## Components

### OverlayBridge

Userspace WireGuard (boringtun) + smoltcp TCP stack. Bridges the macOS host to the
container overlay network over a UDP-over-TCP tunnel to 127.0.0.1.

### eBPF TC Classifiers

Attach TC hooks to intercept and redirect traffic at the kernel level. In the Docker
runtime, uses `nsenter` into the VM's host network namespace. In the host runtime, uses
native aya.

### DNS

Listens on the node's overlay IP, and in the Docker runtime also listens on the bridge
side of the shared `ployz-networking` namespace so overlay workloads can use it as their
container resolver. New overlay workloads are configured to use `ployz-dns` by default.
Those workloads can use short names (`db`) within their namespace or fully-qualified names
(`db.prod.ployz.internal`) across namespaces.

### Gateway

Pingora-based HTTP/TCP reverse proxy. Routes incoming requests by Host header to healthy
service instances discovered from the distributed store. Load balances across replicas.

## Upgrade Contract

The daemon separates cleanly into ephemeral control plane and persistent data plane:

| Component | Restart behavior |
|-----------|-----------------|
| Workloads | Never touched by daemon restart |
| Gateway | Adopted if running and config matches; recreated on drift |
| DNS | Adopted if running and config matches; recreated on drift |
| Corrosion | Adopted if running and parent netns unchanged; recreated on drift |
| WireGuard | Adopted if healthy |
| CLI RPC, remote deploy, heartbeat loops | Ephemeral, restarted with daemon |

### Adopt-First Lifecycle

All managed infrastructure follows the same pattern regardless of runtime mode:

1. Inspect what's already running (by name/unit)
2. Compare identity — a config hash covering the full specification, plus parent
   dependency tracking (e.g. which network namespace container we depend on)
3. If running and identity matches → adopt without touching it
4. If drifted or missing → recreate

Docker containers carry identity as labels (`ployz.config-hash`, `ployz.parent-container-id`).
Systemd units are compared by unit file content. Host user mode always spawns fresh child
processes and makes no persistence guarantees.

## Crate Ownership

The workspace is organized around a small kernel plus concrete backends:

- **`ployz-types`** — pure model/spec/value types
- **`ployz-store-api`** — durable store seam
- **`ployz-runtime-api`** — runtime/network/dataplane/deploy-session seam
- **`ployz-orchestrator`** — deploy policy, manifest export, mesh lifecycle, readiness, join/bootstrap coordination
- **`ployz-corrosion`** — Corrosion-backed store/runtime helpers
- **`ployz-runtime-backends`** — concrete Docker/WireGuard/network/dataplane implementations, plus sidecar supervision and remote deploy transport
- **`ployzd`** — composition root, request handling, CLI/process entry
- **`ployz-gateway` / `ployz-dns`** — edge services over abstract routing-store inputs
- **`ployz-e2e`** — typed daemon-facing test harness

Runtime selection still happens at the daemon composition root, but policy lives below it.
`ployzd` chooses implementations; it does not own deploy policy or backend lifecycle logic.

## Internal Modules

Within those crates, code stays organized by domain rather than adapter buckets:

- **mesh** — WireGuard overlay lifecycle, phase state machine, background sync loops
- **store** — distributed state backends and cluster records
- **network** — non-WireGuard networking (Docker bridge, eBPF classifiers, endpoint discovery)
- **deploy** — preview/apply policy in orchestrator, concrete remote session transport in runtime backends
- **services** — thin gateway/DNS adapters in `ployzd`, concrete sidecar supervision in runtime backends
- **daemon** — request handling and mesh startup orchestration
- **transport** — Unix socket listener

## Future: macOS Host Access

A future `ployzd connect` command for macOS will:

- Spawn a local userspace WireGuard tunnel on macOS
- Spawn a local DNS resolver on macOS
- Give the macOS host direct overlay network access (can reach services by name)
- Not needed for production — only for developer access to the mesh
