# RPC Coordination Philosophy

## Why this exists

Ployz is moving toward an RPC-first coordination model for cluster mutations.
The goal is to remove flappy background loops and replace them with explicit,
transactional operations that either succeed or fail clearly.

This document defines the design intent so future changes stay consistent.

## Core principles

1. **Operation-time truth over periodic reconciliation**
   - Critical control-plane decisions should happen in explicit request/response
     workflows, not asynchronous polling loops.
2. **Durable state is committed intent, not liveness pulses**
   - The replicated store should hold committed cluster intent (membership,
     allocations, routing/deploy records), not heartbeat-style chatter.
3. **One coordination system for all mutation classes**
   - Membership, subnet/IP allocation, and deploy namespace locking should all
     use the same lease/nonce/idempotency model.
4. **Lease ownership and idempotency are mandatory**
   - Every prepare/renew/commit operation must carry a nonce scoped to the
     operation owner.
   - Replays must be safe and deterministic.
5. **No hidden eventual-heal magic in background tasks**
   - Prefer synchronous deny/allow results with explicit retries.

## Latency expectations (theoretical)

These values are planning estimates for global deployments and are not measured
benchmarks.

For a quorum-based two-phase operation (prepare + commit), total latency is
approximately:

- `max_rtt_to_quorum + processing` (prepare)
- `max_rtt_to_quorum + processing` (commit)

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

- parallel quorum RPC fanout,
- strict operation deadlines,
- idempotent retry semantics,
- bounded backoff.

## Allocation policy

Subnet/IP allocation should be proposal-based with race safety:

1. proposer submits `Prepare(subnet, machine, nonce, ttl)`
2. responders `Allow` or `Deny(conflict)`
3. proposer renews while the operation is active
4. proposer commits only after quorum allows
5. commit persists one durable claim in domain state

This prevents dual-winner races through quorum intersection and lease expiry.

## Uniform lock model

Deploy namespace locks should use the same coordination primitives as
membership and subnet claims:

- typed lock keys,
- lease TTL,
- renew,
- owner nonce,
- explicit acquire/commit/release semantics.

This keeps future work obvious and avoids ad-hoc lock implementations.

## Current status

- **Liveness/readiness source:** pulled at decision time via `NodeStatus`.
  We do not persist heartbeat freshness in durable state.
- **Operator intent:** persisted as durable `MachineRecord.drain_state` and surfaced
  through `NodeStatus.drain_state`.
- **Mutations:** subnet claims and deploy namespace locks use quorum
  prepare/renew/commit semantics with owner+nonce idempotency.
- **Background tasks:** focused on reconciliation/subscription work
  (`self_record`, `peer_sync`, endpoint and subnet monitors, eBPF sync) rather
  than liveness pulse publication.
- **MeshReady compatibility:** retained as a compatibility RPC for now while
  `NodeStatus` is the canonical per-node health/readiness contract.
