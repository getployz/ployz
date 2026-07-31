# Control-Plane Backbone

Ployz commits to one control-plane thesis. New control-plane work is measured
against this document. Changing the thesis means superseding it deliberately,
not drifting around it.

## Thesis

Converged beats coordinated. Every machine holds the whole cluster's config
in one shared multi-writer store (Corrosion, last-writer-wins), converged
over the mesh. There is no core, no sequencer, no quorum. The worst failure
class is a coordination point the operator must size, back up, or repair
before they can command their own machines.

Between the two industry defaults — consensus-centric orchestrators, where
the operator inherits quorum, and convergent stores, where concurrent writes
merge by last-writer-wins — Ployz deliberately takes the second, with the
price stated: an earlier config write can silently lose to a later one. In a
single-operator cluster that conflict is the operator racing themselves
inside a sub-second convergence window. Rare, and priced in.

## Row Rules

- Every row has exactly one writer class: operator config rows, or machine
  status rows that only the owning machine writes. A new table names its row
  class and writer in the change that introduces it.
- Rows carry writer identity and timestamp. A fold is surfaced best-effort
  after the fact, never prevented by coordination.
- Keeper converges its machine toward rows it does not own and reports into
  rows nobody else may write. Its authority is exactly those rows; it
  enforces config, it never authors it.
- Docker is execution reality. Status rows are testimony about it, never a
  substitute for it, and no read path infers truth from silence — freshness
  rides mesh handshake age and row timestamps.
- Schema changes are additive-only, primary keys are never-reused ULIDs, and
  full-cluster reseed is the escape hatch and upgrade path.

## Trust Ceiling

Membership is write authority. Admitting a machine — by SSH provisioning or
join token — trusts it with the cluster's config; admission is the security
decision. Operator-signed rows are deferred until hostile-edge or
multi-tenant demand is real, and return as their own effort. The retrofit
stays open by construction: per-row writer identity, additive schema, reseed.

## Availability Contract

- Any machine accepts commands. A command targeting an unreachable machine
  fails instantly with a typed refusal plus mesh last-handshake age; it is
  never queued speculatively.
- Losing a machine never blocks commanding the rest. There is nothing to
  promote, elect, or repair before the cluster is commandable.
- The data plane keeps serving through control-plane loss.
- Machine recovery is reinstall and rejoin; the cluster's config already
  lives on every other machine.

## What This Rejects

- Consensus anywhere in the cluster.
- A core, a sequencer, or any single writer for cluster config.
- Coordination that makes one command wait on another machine's availability.
- Background behavior that authors config rows without an operator write.
- Signing, versioning, or ordering machinery ahead of real demand.

## Guardrails

These are review checks, not aspirations:

- New cluster state is a row with a named writer class, or it does not land.
- A design that needs a coordination point to be correct is redesigned, not
  given one.
- Scale pressure past ~200 machines is answered with cells — many clusters —
  never a bigger cluster; `cluster_id` fields are the only reservation.
