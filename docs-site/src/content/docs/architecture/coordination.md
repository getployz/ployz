---
title: "Cluster coordination with NATS streams and KV leases"
description: "How Ployz uses NATS streams, KV buckets, leases, and request/reply to coordinate explicit operations across cluster nodes without hidden reconcilers."
llms:
  summary: "How Ployz uses NATS streams, KV buckets, leases, and request/reply to coordinate explicit operations across cluster nodes without hidden reconcilers."
---
Ployz uses NATS as its native control-plane substrate. NATS provides durable facts, coordination, request/reply commands, work queues, and scheduled work — but its presence does not justify hidden reconcilers. Every NATS mechanism in Ployz is a vehicle for explicit operations and visible failure surfaces, not a justification for background state rewriting.

## What NATS provides

NATS JetStream gives Ployz four coordination building blocks:

### Durable streams

  Append-only sequences of facts. Deploy commits, routing events, and machine membership changes are published as ordered messages in named streams. Consumers replay them to rebuild state.

### KV buckets

  Mutable key-value stores backed by JetStream. Used for deploy status, instance records, certificates, ACME challenges, and other lifecycle state where the current value matters more than the full history.

### Request/reply

  Single-round-trip RPC over NATS subjects. Used for participant commands during deploy: the orchestrator sends a command to a specific machine's subject and awaits the response. No responder means the machine is unavailable.

### Scheduled messages

  Work queue entries with deferred delivery. Used for certificate renewal scheduling and similar periodic background work that must complete or fail visibly.

## Three kinds of state

Ployz enforces a clear separation across three kinds of cluster state.

### Durable intent

What an operator explicitly asked the cluster to do. Deploy commits are the clearest example: they are immutable messages appended to a stream (`cp_deploy_commits_<authority>`). Once written, they are facts. No background process can silently revise them.

Other durable intent records include machine membership, service revision records, branch lineage, and volume movement evidence.

### Durable status

Mutable lifecycle facts emitted by operations. Deploy status lives in `cp_deploy_status_<authority>` and transitions through defined phases (`applying`, `committed`, `failed`, `FailedAfterCheckpoint`). Instance records in `cp_instances_<authority>` track runtime lifecycle. These are mutable — later operations update them — but every update is an explicit write, not an inferred convergence.

### Live observation

Health, reachability, and freshness observed on demand. Placement probes use NATS request/reply: the orchestrator sends a capacity request to a candidate machine's subject and the machine responds with its current state. No responder, or a timeout, means unavailable now. Ployz does not cache these observations as stored truth.

:::caution
Storing inferred liveness as cluster truth is the primary failure mode this architecture guards against. If a health check result silently becomes a stored policy fact, the cluster can end up serving stale state with no visible signal that something is wrong. Ployz reads liveness at decision time and discards it after the decision is made.
:::

## Coordination commitments

These commitments define what Ployz guarantees about NATS-backed coordination. They are not implementation details — they are observable properties operators can rely on.

### Machine add does not silently change storage authority

Adding a machine to the cluster does not automatically make it a storage authority. Storage authority is an explicit operation with its own preconditions, separate from machine membership. An operator can add capacity without changing which nodes hold control-plane state.

### Quorum and data authority changes are explicit operations

Changing which nodes are trusted with the full control-plane store is a foreground operation that requires explicit operator intent. It is not a side effect of cluster membership changes, node health changes, or background rebalancing.

### Mutating commands fail loudly when peers or preconditions are missing

If a mutating operation requires a peer to be reachable and the peer is not, the operation fails before mutating anything. Ployz does not queue the mutation for eventual delivery or optimistically proceed and hope for reconciliation later. The caller gets a structured failure it can act on.

### Split-brain: refuse writes, not automatic failover

When control-plane write quorum is unavailable, Ployz refuses writes rather than attempting automatic failover. Automatic failover under partition risks creating two active authorities with diverging state. Refusing writes preserves the integrity of what has already been committed and surfaces the problem to the operator.

### Data plane keeps serving last good state when control-plane writes are unavailable

WireGuard tunnels, the gateway, DNS, and running workloads keep operating on their last known-good configuration when `ployzd` is absent or the control plane cannot accept writes. The data plane's job is to serve; it does not stop serving because the control plane is temporarily unavailable.

## Leases and distributed locks

Ployz uses NATS KV-backed leases for two purposes: mutual exclusion during operations, and coordination of scheduled work.

**Deploy locks** prevent concurrent deploys to the same namespace. Before an apply begins, the orchestrator acquires a lease in `cp_locks_<authority>` under the key `cp.lock.deploy.<namespace>`. The lease is held for the duration of the apply and released on completion or failure. A second apply to the same namespace fails immediately with a structured error rather than queuing.

**Other locks** follow the same pattern: certificate issuance acquires `cp.lock.cert.<hostname>`, ACME account operations acquire `cp.lock.acme_account.<issuer_url>`, and subnet reservation acquires `cp.lock.subnet.<subnet>`. Each is a live fact — coordination only, not recorded cluster truth.

:::note
Leases are live coordination state. They are not durable intent. If a node holding a lease crashes, the lease expires and the next operation can proceed. Lease expiry does not create a fact about the failed operation; the operation simply did not complete, and the next caller starts fresh.
:::

## Per-machine request/reply subjects

Remote participant commands use NATS request/reply on per-machine subjects. Subject structure follows the authority hierarchy:

```text
ployz.v1.<installation>.<authority>.rpc.node.<machine_id>.<command>
```

For example, a deploy start-candidate command to a specific node:

```text
ployz.v1.local.auth-default.rpc.node.node-1.deploy.start_candidate
```

Each node listens on a wildcard subject that covers both substrate-level and authority-level commands:

```text
ployz.v1.<installation>.*.rpc.node.<machine_id>.>
```

Nodes within the same queue group share the listener, ensuring only one instance processes each command.

**No responder or timeout is a foreground failure.** Remote mutations never queue. If the target machine is not listening, the foreground operation fails immediately with a structured `RpcFailure`. The caller or operator decides whether to retry.

:::note
This is not fire-and-forget. The orchestrator waits for a reply before proceeding. If the machine cannot respond within the timeout window, the operation fails and leaves no partial state on the target.
:::

## NATS is not a reconciler substrate

NATS streams, KV, and scheduled messages are powerful enough to build a background reconciler on top of. Ployz deliberately does not do this. The rule is:

* Background tasks may publish observations or events.
* Background tasks must not silently rewrite cluster truth.

A scheduled certificate renewal job publishes a work message. The renewal consumer processes it, calls out to ACME, and records the resulting certificate as a durable fact. If it fails, the failure is visible state. At no point does a background task update a machine's membership record, change a deploy's committed state, or alter routing truth without an explicit foreground operation triggering it.

## Store trust boundary

Nodes with `storage=true` are trusted with the full control-plane store. This means they hold JetStream replicas, accept durable writes, and have access to all KV buckets — including material such as TLS private keys, ACME account keys, and invite tokens.

Nodes with `storage=false` receive only the state they need for their runtime role. They connect to NATS as clients, can send and receive messages, but do not host replicated state.

:::caution
Storage-enabled nodes must be treated as trusted with the cluster's secrets. Store data-directory backups contain private key material in effect at the time of the backup. Recovering from a suspected compromise means rotating the affected material — re-issuing certificates, revoking ACME accounts — not just removing the machine from the cluster.
:::

The trust boundary follows the storage flag, not network position or machine role. If a future workload needs a stricter boundary, the right model is scoped NATS subjects and streams with role-specific distribution, not a per-record privacy flag.
