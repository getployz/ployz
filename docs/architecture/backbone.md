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

1. **Docker is execution reality.** What a machine runs is read from Docker,
   and managed containers carry typed labels (service, revision, operation,
   endpoint port) so running reality is self-describing.
2. **The machine fact ledger is machine truth** (ADR-0018). Each machine
   keeps a local SQLite ledger of durable machine-owned facts: route
   attachments applied there, served certificate material, assigned substrate
   state, keeper state, last-known-good gateway/DNS projections. Facts that
   cannot live in immutable container labels — anything applied after deploy
   — live here. The ledger is authoritative for its machine and is never
   cluster truth.
3. **NATS is the authority surface for intent.** Commands, operation records,
   subject permissions, and atomic resource claims (ADR-0015) serialize
   through the core. A command that cannot reach the core fails fast with a
   typed error; it is never queued speculatively on the machine.
4. **JetStream records are classified, every one of them** (ADR-0001): live
   observation, rebuildable index, disposable operation memory, disposable
   job trigger, optional evidence, or explicitly named durable authority.
   Indexes assembled from machine facts and Docker reality are rebuildable by
   construction. Unclassified records are a review failure.
5. **Node-local storage outside the fact ledger and substrate state is cache
   and evidence**, never truth of any kind.

## Machine Fact Ledger Rules

The fact ledger stays simple because these rules are absolute:

- **Single writer.** Only the local daemon writes its ledger, and only as the
  apply step of a typed operation command or a local observation. No peer
  writes another machine's ledger; no background task mutates it.
- **Facts only.** A row answers "what is true on this machine." No workflow
  state, no queues, no cluster policy. If another machine needs to read a row
  directly, the design is wrong — facts travel by being published to the
  core.
- **The fact write is the commit point.** Operations that mutate a machine
  (deploy, route attach, substrate update) commit by writing the machine
  fact; the core KV index row is recorded after and is rebuildable. Claims
  that serialize the operation live in core KV and are disposable.
- **Merge is union plus loud ambiguity.** Facts are namespaced by machine, so
  reassembly is a union. Cluster-scoped conflicts between machines (two
  machines claim the same domain for different services) are detected
  deterministically, surfaced as ambiguous, and left for the operator. Never
  last-write-wins.
- **Schema rides the ployzd version.** Local, additive migrations only; only
  the local binary reads the ledger, so update version-skew rules cover it.

## Availability Contract

- Core unreachable: every operation fails fast with a typed
  control-plane-unreachable error. No partial acceptance, no local queueing.
- Data plane: workloads, gateway, and DNS keep serving last-known-good state
  with freshness visible (ADR-0009). Core loss degrades management, not
  service.
- Core loss: an operator promotes an existing joined machine into the new
  Control-Plane Core through a local recovery command (ADR-0019). Machines
  reconnect or rejoin, publish fresh facts from Docker and their fact ledgers
  — containers, route attachments, served cert material, gateway/DNS
  last-known-good state, and local role authority; an explicit reindex
  operation rebuilds the core's indexes and adopts only unambiguous state
  (ADR-0001). Recovery restores running reality and recorded machine facts,
  not unrealized cluster intent.
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
- Every new fact-ledger table names its owner, its writer, and the operation
  or observation that mutates it, in the change that introduces it.
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
