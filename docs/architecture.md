# Architecture

## Thesis

Ployz is a primitive orchestration core for small clusters.

It turns common infrastructure operations into explicit commands: add capacity,
deploy workloads, move state, branch environments, promote, roll back, and
remove machines. Each command should have visible preconditions, a bounded
effect, a clear result, and a way to verify what happened.

That is the whole architectural bet. Small-scale infrastructure gets better
when the system exposes real operational primitives instead of hiding them
behind policy engines, controllers, and reconciler loops. The same discipline
that makes a CLI good for humans also makes it usable by agents: concrete
actions over a legible system, one operation at a time.

Ployz does not try to be a smaller Kubernetes. It is the orchestration core for
clusters small enough that an operator can understand the whole system, choose
an operation, run it, and verify the result.

## Problem

Running applications across a handful of machines should not require adopting
the operational model of hyperscale infrastructure. Most small clusters need a
few strong capabilities:

- join machines into one private network,
- deploy and route workloads,
- move workloads and persistent state,
- branch and promote environments,
- roll back cleanly,
- remove failed or unwanted capacity,
- and diagnose the system when something is wrong.

The hard part is not exposing every underlying knob. The hard part is shaping
those capabilities into operations that complete or fail honestly. When the
operator has to write scripts to combine low-level steps, the product is
missing a primitive.

## Core Model

### Primitives, Not Policy Engines

Ployz stores enough durable state to execute and explain explicit operations.
It does not store a standing desired-state document that background controllers
continuously reconcile.

Policy belongs at decision time. The operator decides to add, migrate, deploy,
branch, promote, roll back, or remove. Ployz executes that operation with clear
preconditions and returns a result the operator can inspect.

This keeps the cluster boring. There are no autoscalers, hidden schedulers, or
self-healing loops silently rewriting cluster truth.

### Commands, Not Convergence

A mutating operation is foreground work with an audience. It should:

1. inspect current intent and live preconditions,
2. build a plan when the operation has meaningful choices,
3. fail before mutation when preconditions are missing,
4. execute bounded steps against concrete participants,
5. commit durable rows at the point of no return,
6. report cleanup, partial progress, or failure explicitly,
7. leave enough state for safe retry or operator repair.

Retries must not turn uncertainty into success. A failed operation remains a
fact until a later operation resolves it.

### Intent, Status, And Observation

Ployz separates three kinds of truth:

- **Intent**: what an operator explicitly asked the cluster to do.
- **Status**: durable lifecycle rows emitted by operations.
- **Observation**: live reachability, health, capacity, and freshness checked
  at decision time.

Durable state should not infer liveness. Observations may be cached for
diagnostics, but they do not silently become cluster policy.

### Disposable Daemon, Durable Data Plane

`ployzd` is disposable control plane. It can crash, upgrade, or restart without
disrupting the data plane. WireGuard tunnels stay up, Corrosion keeps serving replicated state,
the gateway keeps proxying, DNS keeps resolving, and workload containers keep
running.

On startup, the daemon adopts what is already running and only recreates
managed infrastructure whose identity has drifted.

## System Boundaries

### Operator Surfaces

The CLI, SDK, API, cloud UI, and agents are all consumers of the same
primitive surface. None of them are the source of cluster truth.

Human ergonomics matter, but the architectural contract is stronger than a
human-friendly wrapper: operations need structured output, typed failures,
idempotent retry behavior, and explicit verification hooks.

### Orchestration Kernel

Core orchestration owns product semantics: machine membership, placement,
deploy lifecycle, migration, transfer, branch, promote, rollback, coordination,
and diagnostic policy.

The kernel depends on narrow contracts for runtime, store, network, storage,
and service supervision. It does not reach upward into CLI, cloud, or agent
flows for convenience.

### Runtime And Substrate Backends

Backends own substrate mechanics:

- Docker or host runtime operations,
- WireGuard setup and peer configuration,
- Corrosion process and schema management,
- ZFS or other storage mechanics,
- gateway and DNS process supervision,
- eBPF or bridge networking details.

Backends implement explicit contracts. They do not decide product policy.

### Data Plane Services

The data plane is the set of services that must keep serving last good state
when `ployzd` is absent:

- workload containers,
- WireGuard mesh,
- Corrosion,
- gateway,
- DNS,
- storage datasets and volumes.

Daemon restart must not restart workloads. Managed sidecars may be adopted,
recreated on drift, or repaired by explicit operation, but they are not treated
as ephemeral children of a single daemon process.

## Runtime Model

The public runtime surface is split across two axes:

| Runtime target | Service mode | Meaning |
|----------------|--------------|---------|
| Docker | User | Docker-backed mesh/store/sidecars with loopback control-plane binding |
| Host | User | Host-backed mesh/store, child-process sidecars, overlay control-plane binding |
| Host | System | Host-backed mesh/store, system-managed sidecars, overlay control-plane binding |

`Memory` is test-only. It is not an operator-facing runtime and does not shape
the daemon's public API.

Runtime selection happens at the daemon composition root. Core domains receive
explicit backends instead of matching on an operator-facing mode enum.

## Core Domains

Code is organized by domain, not by adapter pattern.

- **machine**: machine identity, membership, join, update, remove, and operator
  surfaces for capacity.
- **mesh**: WireGuard overlay lifecycle, peer state, subnet coordination, and
  mesh phase state.
- **store**: durable Corrosion rows, subscriptions, leases, and memory test
  implementations.
- **coordination**: leases, participant commands, explicit foreground
  coordination, and failure reporting.
- **deploy**: preview, placement, participant probing, apply, commit, cleanup,
  and deploy lifecycle rows.
- **runtime**: local container/process operations through narrow backend
  contracts.
- **storage**: volume creation, snapshot, clone, transfer, receive, migration,
  and rollback mechanics.
- **routing**: route rows, gateway projection, DNS projection, and freshness
  handling.
- **services**: long-lived sidecar supervision for Corrosion, gateway, DNS, and
  supporting processes.
- **daemon**: composition root, request handling, startup adoption, and
  operation dispatch.
- **SDK/API**: external command surface and structured request/response types.

WireGuard implementations live under the mesh domain because mesh owns overlay
lifecycle. Store backends live under the store domain because store owns
distributed state. Runtime backends live below the orchestration kernel because
runtime mechanics are not product policy.

## State And Coordination

Corrosion is the native replicated-state substrate. It provides durable rows,
subscriptions, resumable change cursors, and operator-visible state surfaces.
Iroh peer RPC provides bounded foreground daemon-to-daemon commands.

important architectural commitments are:

- machine add does not silently change storage authority,
- quorum and data authority changes are explicit operations,
- mutating commands fail loudly when peers or preconditions are missing,
- split-brain risk is handled operationally by visible conflicts and failed
  preflights, not by automatic failover,
- the data plane keeps serving last good runtime state when control-plane
  writes are unavailable.

Corrosion is not a command bus and is not a justification for hidden
reconcilers. Corrosion rows/subscriptions are mechanisms for replicated state;
bounded iroh RPC is the command path for concrete peer work.

## Routing And Deploys

Deploy and routing semantics are described in `docs/routing-and-deploys.md`.
The baseline rule is that traffic only sees committed, routable rows.

The longer deploy primitive direction is described in
`docs/architecture/deploy-primitives-roadmap.md`. In that model, deploy is the
compiler for explicit operations such as branching, portal references,
migration, promotion, and machine drain: high-level commands produce typed,
phase-aware deploy plans with visible preflights, commit boundaries, rollback
policy, and durable evidence.

Deploys should move through visible phases: plan, apply, commit, cleanup, or
fail. New instances can be started before commit, but routing flips only after
the durable commit point. Cleanup failures become explicit recoverable status;
they do not erase the fact that the new version is live.

Gateway and DNS rebuild from durable routing state first, then consume ordered
routing events. If freshness becomes uncertain, they reload rather than serving
silent stale projections.

## Upgrade And Adoption Contract

The daemon separates ephemeral control-plane work from persistent data-plane
services:

| Component | Restart behavior |
|-----------|------------------|
| Workloads | Never touched by daemon restart |
| Gateway | Adopted if running and config matches; recreated on drift |
| DNS | Adopted if running and config matches; recreated on drift |
| Corrosion | Adopted if running and data directory matches; recreated on explicit repair |
| WireGuard | Adopted if healthy |
| CLI RPC, remote deploy, background command listeners | Ephemeral, restarted with daemon |

All managed infrastructure follows the same adopt-first lifecycle:

1. inspect what is already running,
2. compare identity against the full expected specification,
3. adopt matching infrastructure without touching it,
4. recreate missing or drifted infrastructure with visible status.

Docker containers carry identity as labels such as `ployz.config-hash` and
`ployz.parent-container-id`. System services compare rendered unit identity.
Host user mode may spawn fresh child processes and makes no persistence
guarantees beyond the selected mode's contract.

## Docker Runtime On macOS

The daemon runs on the macOS host. Everything else runs inside Docker Desktop's
Linux VM. Corrosion, gateway, and DNS bind on the node's overlay IPv6 address so
other mesh nodes can reach them directly. In the Docker runtime they share the
`ployz-networking` network namespace to access `wg0`.

```text
macOS host                         Docker Desktop VM
+----------------+                 +------------------------------+
| ployzd daemon  |                 | ployz-networking container   |
|                |  WG bridge      |   wg0 overlay interface      |
| OverlayBridge  +---------------->|                              |
|                |                 | corrosion                    |
| Store bridge   +---------------->| ployz-gateway                |
|                |                 | ployz-dns                    |
|                |                 | workload containers          |
+----------------+                 +------------------------------+
```

`OverlayBridge` uses userspace WireGuard and a smoltcp TCP stack to bridge the
macOS host to the container overlay network. eBPF TC classifiers intercept and
redirect traffic at the kernel level where the runtime supports it.

## Endpoint Ordering

Published machine endpoints are not an arbitrary interface dump. Ordering is
part of transport behavior because it becomes the candidate order for WireGuard
endpoint selection and rotation.

The rules are:

1. Drop unusable addresses entirely:
   - loopback,
   - link-local,
   - IPv6 ULA,
   - interfaces below the minimum MTU required for the overlay,
   - container, bridge, and helper interfaces that are not cluster-facing.
2. Order the remaining addresses by likely usefulness:
   - private RFC1918 first,
   - CGNAT second,
   - public addresses after that.

Public-IP discovery is folded into the same ordering instead of being forced to
the front. That keeps directly routable private paths ahead of broader internet
paths while still advertising NAT-discovered public reachability when needed.

## Future: Host Access From macOS

A future `ployzd connect` command for macOS should:

- spawn a local userspace WireGuard tunnel,
- spawn a local DNS resolver,
- give the macOS host direct overlay network access,
- remain a developer access feature, not a production dependency.

## Design Test

When changing the architecture, ask:

- Does this create a new primitive or hide a procedure behind policy?
- Can the operation fail before mutation when preconditions are missing?
- Does durable state record intent and lifecycle facts rather than inferred
  liveness?
- Can a human or agent verify the result without knowing hidden background
  behavior?
- Does daemon restart leave the data plane serving last good state?
- Does the design keep local, self-hosted, cloud, and future agent surfaces on
  one model?

If the answer is no, the design is probably adding orchestration machinery
where Ployz should be adding a better primitive.
