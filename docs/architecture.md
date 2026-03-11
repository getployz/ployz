# Architecture

## The Core Idea

ployzd is disposable control plane. It can crash, upgrade, restart — and nothing in the
data plane notices. WireGuard tunnels stay up, Corrosion keeps replicating, the gateway
keeps proxying, DNS keeps resolving, and workload containers keep running. On startup
the daemon attaches to whatever is already running and only recreates things whose
configuration has drifted.

This is the north star. Every design decision flows from it.

## Runtime Modes

The same daemon binary runs in four modes. Each mode provides different implementations
of the same abstractions (WireGuard, store, eBPF, sidecar services):

| Mode | Target | Key trait |
|------|--------|-----------|
| Memory | Testing | Everything in-process, no real networking, no containers |
| Docker | macOS | Infrastructure runs as containers inside Docker Desktop's Linux VM |
| HostExec | Linux dev | Infrastructure runs as child processes of the daemon |
| HostService | Linux prod | Infrastructure runs as systemd units |

Memory mode is first-class — it's how tests run without Docker or Linux. Gateway and DNS
return noop handles in memory mode; there's nothing to proxy to.

## Docker Mode (macOS)

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
│                 │                  │   DNS server on [overlay]:53      │
│                 │                  │                                   │
│                 │                  │ workload containers               │
│                 │                  │   Docker bridge network           │
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

Attach TC hooks to intercept and redirect traffic at the kernel level. In Docker mode,
uses `nsenter` into the VM's host network namespace. In host modes, uses native aya.

### DNS

Listens on the node's overlay IP. Resolves service names to instance IPs using routing
state from the distributed store. Containers can use short names (`db`) within their
namespace or fully-qualified names (`db.prod.ployz.internal`) across namespaces.

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
Systemd units are compared by unit file content. HostExec mode always spawns fresh — it's
for development and makes no persistence guarantees.

## Module Organization

Code is organized by domain, not by adapter pattern. WireGuard implementations live under
the mesh domain because mesh owns the overlay lifecycle. Store backends live under the
store domain because store owns distributed state. Each domain has a driver enum that
dispatches across runtime modes.

The key domains:
- **mesh** — WireGuard overlay lifecycle, phase state machine, background sync loops
- **store** — distributed state (Corrosion backends, memory backend, bootstrap, network config)
- **network** — non-WireGuard networking (Docker bridge, eBPF classifiers, endpoint discovery)
- **services** — long-lived sidecar management (supervisor lifecycle, gateway, DNS)
- **deploy** — workload deployment (preview/apply coordination, container CRUD, remote sessions)
- **daemon** — request handling, mesh startup orchestration
- **node** — machine identity
- **transport** — Unix socket listener

## Future: macOS Host Access

A future `ployzd connect` command for macOS will:

- Spawn a local userspace WireGuard tunnel on macOS
- Spawn a local DNS resolver on macOS
- Give the macOS host direct overlay network access (can reach services by name)
- Not needed for production — only for developer access to the mesh
