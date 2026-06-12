# Control-Plane Backbone

Ployz commits to one control-plane thesis. New control-plane work is measured
against this document. Changing the thesis means superseding the ADRs it
cites, not drifting around them.

## Thesis

Machines own their runtime truth. The control plane is one disposable core.
Nothing in the cluster runs consensus. When the core is unavailable,
operations fail loudly and the data plane keeps serving. Recovery is a
bounded operation, not quorum repair.

This is a deliberate position between two industry defaults:

- Consensus-centric orchestrators put a Raft-replicated database at the
  center of the cluster. The cluster survives node loss, but the operator
  inherits quorum: sizing it, backing it up, and repairing it before they can
  touch their own application.
- Gossip/CRDT orchestrators let any partition accept writes and merge later.
  The operator can always issue commands, but conflicting truth merges
  silently. Silent divergence is the worst failure class this product
  recognizes.

Ployz takes neither. There is no quorum to repair and no merge to trust. The
core is a rendezvous and a rebuildable index; the machines are the facts.

## Authority Ladder

1. **Docker and machine-local substrate state are execution reality.** What a
   machine runs, its keeper state, its release source, and its substrate lock
   live on that machine and are authoritative for it.
2. **NATS is the authority surface for intent.** Commands, operation records,
   subject permissions, and atomic resource claims (ADR-0015) serialize
   through the core. A command that cannot reach the core fails fast with a
   typed error; it is never queued speculatively on the machine.
3. **JetStream records are classified, every one of them** (ADR-0001): live
   observation, rebuildable index, disposable operation memory, disposable
   job trigger, optional evidence, or explicitly named durable authority.
   Unclassified records are a review failure.
4. **Node-local storage outside substrate state is cache and evidence**, never
   cluster truth.

## Availability Contract

- Core unreachable: every operation fails fast with a typed
  control-plane-unreachable error. No partial acceptance, no local queueing.
- Data plane: workloads, gateway, and DNS keep serving last-known-good state
  with freshness visible (ADR-0009). Core loss degrades management, not
  service.
- Core loss: machines reconnect or rejoin, publish fresh signed facts from
  Docker, keeper state, gateway/DNS last-known-good state, and local role
  authority; an explicit reindex operation rebuilds the core's indexes and
  adopts only unambiguous state (ADR-0001).
- The reindex operation is part of this backbone. Until it exists and is
  exercised end-to-end (destroy the core's JetStream state, stand up a core,
  reindex, verify), JetStream loss is unrecoverable in practice and the
  thesis is not implemented. It is a blocking v1 deliverable, not a
  follow-up.

## What This Rejects

- Consensus anywhere in the cluster, including replicated JetStream
  (ADR-0016). Replication factor stays 1.
- A gossip/CRDT cluster store. Partition-tolerant writes require silent
  merge, and silent merge converts stale state into truth.
- Background reconcilers that rewrite cluster policy. Observation is not
  reconciliation.
- Rollout, scheduling, and fleet-automation engines in the core (ADR-0017).
  Drivers above the core sequence the same single-machine primitives.

## Guardrails

These are review checks, not aspirations:

- Every new KV bucket, stream, or object store bucket names its ADR-0001
  classification in the change that introduces it.
- No JetStream resource sets replicas above one. Raising replication requires
  superseding ADR-0016 with a design that answers quorum operations
  end-to-end.
- Cluster-scoped invariants (idempotency, resource locks, ordered operation
  timelines) serialize through core primitives, never through hand-rolled
  coordination on machines.
- Machine-scoped truth (release source, assigned substrate state, machine
  substrate lock) stays on the machine and is never promoted to a core-owned
  record.
- Automation layers — cloud workflows, agents, scripts — drive the same
  operations the CLI drives. Safety preconditions (version skew, locks,
  idempotency, exact versions) are enforced in the core so no driver can
  retry its way into an unsafe state.
