# Control-Plane Backbone

Ployz commits to one control-plane thesis. New control-plane work is measured
against this document. Changing the thesis means superseding it deliberately,
not drifting around it.

## Thesis

Converged beats consensus. Coordination stays disposable. Every machine holds
the whole cluster's config in one shared multi-writer store (Corrosion,
last-writer-wins), converged over the mesh. There is no replicated core,
sequencer, or quorum. One preferred controller serializes mutations in the
healthy case with ordinary in-process exclusion, but its appointment and
unfinished work may be discarded rather than restored or migrated. Each node
uses local durable execution only for effects on that node.

Between the two industry defaults — consensus-centric orchestrators, where
the operator inherits quorum, and convergent stores, where concurrent writes
merge by last-writer-wins — Ployz deliberately takes the second, with the
price stated: an earlier config write can silently lose to a later one. In a
single-operator cluster that conflict is the operator racing themselves
inside a sub-second convergence window. Rare, and priced in.

## Row Rules

- Every ordinary row has one named writer class: an explicit command or the
  machine that owns the testimony. A new table names its row class and writer
  in the change that introduces it.
- The Controller Appointment is the single named exception: any API machine
  passing the visibility brake may replace the advisory row immediately after
  one hard connect failure. Timeouts, HTTP responses, and protocol failures do
  not replace it; Corrosion LWW resolves concurrent appointments.
- Ordinary product rows carry writer identity and timestamp. The structural
  Controller Appointment carries only machine and opaque appointment identity.
  A fold is surfaced best-effort after the fact, never prevented by
  coordination.
- Keeper converges its machine toward rows it does not own and reports into
  rows nobody else may write. Its authority is exactly those rows; it
  enforces config, it never authors it.
- Docker is execution reality. Status rows are testimony about it, never a
  substitute for it, and no read path infers truth from silence — freshness
  rides mesh handshake age and row timestamps.
- Schema changes are additive-only, primary keys are canonical names or named composites, and
  full-cluster refound is the escape hatch and upgrade path.

## Trust Ceiling

Membership is write authority. Admitting a machine — by SSH provisioning or
join token — trusts it with the cluster's config; admission is the security
decision. Operator-signed rows are deferred until hostile-edge or
multi-tenant demand is real, and return as their own effort. The retrofit
stays open by construction: per-row writer identity, additive schema, reseed.

## Availability Contract

- Any machine accepts commands. Followers forward mutations to the current
  preferred controller within a bounded budget.
- A follower that gets one hard connect failure may immediately take a new
  advisory appointment; it does not wait on a persistent-failure timer.
  Timeouts and application/protocol responses leave the appointment alone.
  Losing the controller interrupts its unfinished in-memory attempt, and the
  caller retries from Corrosion and host reality; there is no controller
  history to migrate or replicated controller service to repair.
- Each node's Duroxide/SQLite history belongs only to host-local prepare and
  retire effects. It resumes on that node and never elects or replaces a
  controller.
- A singleton cluster may mutate. In a rostered multi-machine cluster, a
  controller must see at least two Corrosion members. This is not quorum and
  equal partitions may both operate.
- The data plane keeps serving through control-plane loss.
- Machine recovery is reinstall and rejoin; the cluster's config already
  lives on every other machine.

## What This Rejects

- Consensus anywhere in the cluster.
- A replicated core, sequencer, quorum, or consensus-elected writer.
- Durable cluster-coordination state that must be recovered before commands
  resume.
- Background behavior that authors config rows without an operator write.
- Signing, versioning, or ordering machinery ahead of real demand.

## Guardrails

These are review checks, not aspirations:

- New cluster state is a row with a named writer class, or it does not land.
- A coordination point may simplify the healthy case only when losing or
  splitting it has an explicit recoverable result and does not endanger stored
  data.
- Scale pressure past ~200 machines is answered with cells — many clusters —
  never a bigger cluster; `cluster_id` fields are the only reservation.
