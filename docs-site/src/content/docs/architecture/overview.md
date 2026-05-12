---
title: "Ployz architecture overview: primitives over policy"
description: "How Ployz turns operations into explicit commands with visible preconditions, bounded effects, and verifiable results — no reconcilers, no hidden policy."
llms:
  summary: "How Ployz turns operations into explicit commands with visible preconditions, bounded effects, and verifiable results — no reconcilers, no hidden policy."
---
Ployz is a primitive orchestration core for small clusters. Its architectural bet is simple: small-scale infrastructure gets better when the system exposes real operational primitives instead of hiding them behind policy engines, controllers, and reconciler loops. Every state change is an explicit operation — add a machine, deploy a workload, migrate state, branch an environment, promote, roll back — with visible preconditions, a bounded effect, a clear result, and a way to verify what happened.

## The core model

Ployz stores enough durable state to execute and explain explicit operations. It does not store a standing desired-state document that background controllers continuously reconcile.

**Policy belongs at decision time.** The operator decides to add, migrate, deploy, branch, promote, roll back, or remove. Ployz executes that operation with clear preconditions and returns a result the operator can inspect. There are no autoscalers, hidden schedulers, or self-healing loops silently rewriting cluster truth.

A mutating operation is foreground work with an audience. It should:

1. Inspect current intent and live preconditions
2. Build a plan when the operation has meaningful choices
3. Fail before mutation when preconditions are missing
4. Execute bounded steps against concrete participants
5. Commit durable facts at the point of no return
6. Report cleanup, partial progress, or failure explicitly
7. Leave enough state for safe retry or operator repair

Retries must not turn uncertainty into success. A failed operation remains a fact until a later operation resolves it.

### Three kinds of state

Ployz separates three kinds of truth:

* **Intent** — what an operator explicitly asked the cluster to do
* **Status** — durable lifecycle facts emitted by operations
* **Observation** — live reachability, health, capacity, and freshness checked at decision time

Durable state does not infer liveness. Observations may be cached for diagnostics, but they do not silently become cluster policy.

### Disposable daemon, durable data plane

`ployzd` is a disposable control plane. It can crash, upgrade, or restart without disrupting the data plane. WireGuard tunnels stay up, NATS keeps serving state, the gateway keeps proxying, DNS keeps resolving, and workload containers keep running.

On startup, the daemon adopts what is already running and only recreates managed infrastructure whose identity has drifted.

## System boundaries

Ployz is organized into four layers that interact through explicit contracts.

### Operator surfaces

  CLI, SDK, API, cloud UI, and agents. All consumers of the same primitive surface. None are the source of cluster truth. Operations need structured output, typed failures, idempotent retry behavior, and explicit verification hooks.

### Orchestration kernel

  Owns product semantics: machine membership, placement, deploy lifecycle, migration, transfer, branch, promote, rollback, coordination, and diagnostic policy. Depends on narrow contracts for runtime, store, network, and storage.

### Runtime and substrate backends

  Own substrate mechanics: Docker or host runtime operations, WireGuard setup, NATS process management, ZFS or other storage, gateway and DNS process supervision, and eBPF or bridge networking. Backends implement explicit contracts. They do not decide product policy.

### Data plane services

  The set of services that must keep serving last good state when `ployzd` is absent: workload containers, WireGuard mesh, NATS, gateway, DNS, and storage datasets. Daemon restart must not restart workloads.

## Core domains

Code is organized by domain, not by adapter pattern.

| Domain         | Responsibility                                                                                    |
| -------------- | ------------------------------------------------------------------------------------------------- |
| `machine`      | Machine identity, membership, join, update, remove, and operator surfaces for capacity            |
| `mesh`         | WireGuard overlay lifecycle, peer state, subnet coordination, and mesh phase state                |
| `store`        | Durable cluster facts, subscriptions, locks, streams, KV records, and memory/NATS implementations |
| `coordination` | Leases, participant commands, explicit foreground coordination, and failure reporting             |
| `deploy`       | Preview, placement, participant probing, apply, commit, cleanup, and deploy lifecycle facts       |
| `runtime`      | Local container/process operations through narrow backend contracts                               |
| `storage`      | Volume creation, snapshot, clone, transfer, receive, migration, and rollback mechanics            |
| `routing`      | Route facts, gateway projection, DNS projection, and freshness handling                           |
| `services`     | Long-lived sidecar supervision for NATS, gateway, DNS, and supporting processes                   |
| `daemon`       | Composition root, request handling, startup adoption, and operation dispatch                      |
| `SDK/API`      | External command surface and structured request/response types                                    |

WireGuard implementations live under the mesh domain because mesh owns overlay lifecycle. Store backends live under the store domain because store owns distributed state. Runtime backends live below the orchestration kernel because runtime mechanics are not product policy.

## Runtime targets

Runtime selection happens at the daemon composition root. Core domains receive explicit backends instead of matching on an operator-facing mode enum.

| Runtime target | Service mode | Meaning                                                                        |
| -------------- | ------------ | ------------------------------------------------------------------------------ |
| Docker         | User         | Docker-backed mesh/store/sidecars with loopback control-plane binding          |
| Host           | User         | Host-backed mesh/store, child-process sidecars, overlay control-plane binding  |
| Host           | System       | Host-backed mesh/store, system-managed sidecars, overlay control-plane binding |

:::note
`Memory` is test-only. It is not an operator-facing runtime and does not shape the daemon's public API.
:::

## Docker runtime on macOS

The daemon runs on the macOS host. Everything else runs inside Docker Desktop's Linux VM. NATS, gateway, and DNS bind on the node's overlay IPv6 address so other mesh nodes can reach them directly. In the Docker runtime they share the `ployz-networking` network namespace to access `wg0`.

```text
macOS host                         Docker Desktop VM
+----------------+                 +------------------------------+
| ployzd daemon  |                 | ployz-networking container   |
|                |  WG bridge      |   wg0 overlay interface      |
| OverlayBridge  +---------------->|                              |
|                |                 | nats-server                  |
| NATS bridge    +---------------->| ployz-gateway                |
|                |                 | ployz-dns                    |
|                |                 | workload containers          |
+----------------+                 +------------------------------+
```

`OverlayBridge` uses userspace WireGuard and a smoltcp TCP stack to bridge the macOS host to the container overlay network. eBPF TC classifiers intercept and redirect traffic at the kernel level where the runtime supports it.

## Upgrade and adoption contract

The daemon separates ephemeral control-plane work from persistent data-plane services.

| Component                                            | Restart behavior                                                  |
| ---------------------------------------------------- | ----------------------------------------------------------------- |
| Workloads                                            | Never touched by daemon restart                                   |
| Gateway                                              | Adopted if running and config matches; recreated on drift         |
| DNS                                                  | Adopted if running and config matches; recreated on drift         |
| NATS                                                 | Adopted if running and parent netns unchanged; recreated on drift |
| WireGuard                                            | Adopted if healthy                                                |
| CLI RPC, remote deploy, background command listeners | Ephemeral, restarted with daemon                                  |

All managed infrastructure follows the same adopt-first lifecycle: inspect what is already running, compare identity against the full expected specification, adopt matching infrastructure without touching it, recreate missing or drifted infrastructure with visible status.

Docker containers carry identity as labels such as `ployz.config-hash` and `ployz.parent-container-id`. System services compare rendered unit identity.

## Explore further

### [Cluster coordination with NATS](/architecture/coordination)

  How NATS acts as the control-plane substrate: streams, KV buckets, leases, distributed locks, and the commitments that prevent split-brain and hidden state changes.

### [Routing, gateway, and DNS](/architecture/routing)

  How deploy truth is modeled, how the apply flow commits facts at points of no return, and how the gateway and DNS rebuild from durable routing state.

## Design test

When evaluating a proposed change to Ployz architecture, ask these questions:

### Does this create a new primitive or hide a procedure behind policy?

  Ployz primitives are explicit commands with visible preconditions and bounded effects. If a feature encodes decisions into the cluster so that they happen without the operator choosing them, it is adding policy, not a primitive. Prefer the primitive.

### Can the operation fail before mutation when preconditions are missing?

  A well-formed operation inspects intent and live preconditions first, builds a plan, and fails cleanly before touching anything if preconditions are not met. An operation that starts mutating before validating creates partial-state problems that are harder to recover from than a clean upfront failure.

### Does durable state record intent and lifecycle facts rather than inferred liveness?

  Stored state should represent what an operator asked for and what explicitly happened. Health, reachability, and freshness are observed live at decision time. Storing inferred liveness as cluster truth leads to stale state serving silently — the worst failure class.

### Can a human or agent verify the result without knowing hidden background behavior?

  The system should be fully legible from its observable state. A verifiable result means the operator (or any automation) can confirm the outcome by reading durable facts, not by knowing that a reconciler will eventually make it true. If verification requires waiting for background convergence, the primitive is not done.

### Does daemon restart leave the data plane serving last good state?

  `ployzd` is disposable. Any design that causes a daemon restart to interrupt WireGuard, NATS, the gateway, DNS, or running workloads has broken the separation between control plane and data plane. The daemon adopts; it does not own the data plane's lifecycle.

### Does the design keep local, self-hosted, cloud, and future agent surfaces on one model?

  A developer running `ployzctl dev` on a Mac and a fleet operator running production share the same primitives. There is no dev-mode shortcut and no cloud-only mechanism. If a feature requires a separate model for one of these surfaces, the primitive needs to be strengthened, not forked.

:::tip
If the answer to any design test question is no, the design is probably adding orchestration machinery where Ployz should be adding a better primitive.
:::
