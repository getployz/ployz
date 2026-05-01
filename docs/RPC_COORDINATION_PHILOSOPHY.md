# NATS-Native Coordination Philosophy

## Why this exists

Ployz is moving toward a NATS-native control plane for cluster mutations.
The goal is to remove peer-vote coordination, bespoke TCP command protocols,
and background correction loops. NATS should be the first design tool for
state, coordination, command delivery, and failure visibility.

This document defines the design intent so future changes stay consistent.

## Core principles

1. **Operation-time truth over periodic reconciliation**
   - Critical control-plane decisions should happen in explicit request/response
     workflows, not asynchronous polling loops.
2. **Durable state is committed intent, not liveness pulses**
   - The replicated store should hold committed cluster intent (membership,
     allocations, routing/deploy records), not heartbeat-style chatter.
3. **NATS primitives before custom protocols**
   - Use JetStream streams for ordered immutable facts.
   - Use KV CAS writes for single-key authority and short leases.
   - Use request/reply for bounded daemon commands.
   - Use work queues and scheduled messages for exactly-one background work.
   - Keep direct TCP only for true byte streams such as ZFS send payloads.
4. **Lease ownership and idempotency are mandatory**
   - Every lease acquisition, renew, release, and side-effecting command must
     carry an operation-scoped nonce or idempotency key.
   - Replays must be safe and deterministic.
5. **No hidden eventual-heal magic in background tasks**
   - Prefer command-time success/failure, explicit retry, and observable
     degraded state over silent convergence.

## Target Control Plane

- **Durable state:** KV for independent current records; streams for ordered
  facts and audit.
- **Locks/leases:** NATS KV CAS leases with explicit owner, nonce, expiry, and
  stale-release protection.
- **Node commands:** NATS request/reply on `node.<machine>.cmd.>` subjects.
  No-responder and timeout failures are surfaced to the caller.
- **Work dispatch:** JetStream work queues with explicit ack and bounded
  redelivery.
- **Timed work:** scheduled NATS messages, not daemon-local tickers, when NATS
  can own the wake-up.
- **Streaming:** direct TCP remains only for bulk transfer paths where the
  protocol is a byte stream, not request/reply control.

## Failure Model

| Scenario | Expected behavior |
|----------|-------------------|
| Single node | R=1. Writes are available while the node is up. No HA claim. |
| Machine add | Adds a member and connectivity. It does not change JetStream replica count or storage authority. |
| Explicit R=3 promotion | Operator selects three eligible storage candidates. The command validates guardrails, then reconfigures NATS assets. |
| 3-node HA | After explicit R=3 promotion, one storage candidate may disappear and writes continue after leader election. |
| Below quorum | JetStream writes fail loudly. Operators see blocked mutations; data plane serves last good runtime state. |
| Offline non-storage node | NATS request/reply returns no responders or timeout. Command fails to the caller. |
| Offline storage candidate with quorum | Writes continue after leader election; rejoin catches up through JetStream. |
| Planned node removal | Operator demotes/removes storage responsibility before membership removal when quorum would be degraded. |
| Unplanned node loss | Stored membership remains intent. Status surfaces loss; no background policy silently rewrites placement. |
| Explicit upgrade | Operator upgrades nodes one at a time, checking NATS health/quorum and data-plane continuity between steps. |
| High latency region | Writes pay quorum RTT. Prefer regional R=3 hubs plus async mirrors over cross-region quorum by default. |
| Region loss | Writes move only through explicit operator failover or mirror promotion; split-brain prevention beats automatic takeover. |

## Storage Promotion Guardrails

Storage promotion is a first-class operator operation. It is not triggered by
machine add, peer count, or background reconciliation.

A promotion to R=3 or R=5 must validate:

- the selected candidate count exactly matches the target replica count,
- all candidates are active, non-draining, reachable through NATS, and not in an
  upgrade/remove/bootstrap operation,
- each candidate has persistent JetStream storage and enough free capacity,
- route RTT/loss fit the selected latency class,
- declared region/AZ/failure-domain diversity is sufficient for the target,
- the current stream/KV assets can reconfigure from their existing replica set
  without dropping below quorum,
- rollback/demotion instructions are printed before any irreversible step.

Failures are foreground failures to the operator. The daemon must not partially
promote a node and present the operation as successful.

## Latency expectations

These values are planning estimates and must be replaced by e2e measurements
as the NATS-native E2E suite grows.

For a NATS KV or stream write, latency is approximately:

- `max_rtt_to_fastest_quorum + broker processing`

Typical global ranges:

- p50: ~220-380ms
- p95: ~450-900ms
- p99 tail in stressed scenarios: ~1.1-1.8s

### N+1 tail scenarios

Tail spikes typically come from one extra slow component:

- one overloaded coordinator,
- one transoceanic route with jitter,
- one retransmit in the commit phase,
- one lease refresh path delayed by GC/scheduler pauses.

The design should isolate these tails through:

- NATS quorum writes instead of application fanout,
- strict operation deadlines,
- idempotent retry semantics,
- bounded backoff.

## Allocation policy

Subnet/IP allocation should be NATS KV lease backed:

1. proposer acquires `locks.subnet.<subnet>` with CAS create or expired-revision update
2. proposer validates current durable machine/subnet records
3. proposer writes the durable claim
4. proposer releases the lease by expected revision and nonce

This prevents dual-winner races through broker CAS rather than peer voting.

## Uniform lock model

Deploy namespace locks, certificate issuance locks, account creation locks,
and subnet locks should use the same NATS lease shape:

- typed lock keys,
- lease TTL,
- renew,
- owner nonce,
- explicit acquire/commit/release semantics.

This keeps future work obvious and removes ad-hoc lock implementations.

## Current status

- **Liveness/readiness source:** pulled at decision time via `NodeStatus`.
  We do not persist heartbeat freshness in durable state.
- **Operator intent:** persisted as durable `MachineRecord.drain_state` and surfaced
  through `NodeStatus.drain_state`.
- **Mutations:** deploy, cert, account, and subnet coordination are moving to
  NATS KV CAS leases with owner+nonce idempotency.
- **Node command path:** NATS request/reply is the path for small bounded daemon
  commands. The old peer TCP RPC client and peer control listener have been
  removed; direct TCP remains only for narrow byte streams such as ZFS transfer
  payloads.
- **Background tasks:** focused on reconciliation/subscription work
  (`self_record`, `peer_sync`, endpoint and subnet monitors, eBPF sync) rather
  than liveness pulse publication.
- **MeshReady compatibility:** retained as a compatibility RPC for now while
  `NodeStatus` is the canonical per-node health/readiness contract.
