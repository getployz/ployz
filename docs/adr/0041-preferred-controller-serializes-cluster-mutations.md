# A Preferred Controller Serializes Cluster Mutations

Ployz keeps Corrosion as the replicated intent-and-publication surface and adds
one small, disposable coordination point. A singleton Corrosion row names the
preferred controller. Every machine still serves the API; a follower forwards
cluster mutations to the named machine over bounded HTTP.

The preferred controller is ordinary async Rust guarded by one in-memory
mutation lock. It observes current rows and target hosts, computes a plan, and
dispatches bounded effects. It has no durable queue or workflow history.
Overlapping mutations may be refused as `controller_busy` instead of waiting in
a persisted scheduler.

This is not a return to the Core. Corrosion remains the only replicated
publication mechanism, while nodes answer live runtime questions from Docker
over bounded RPC. There is no consensus leader, quorum, sequencer, claim
service, or NATS.

## Controller appointment

The `controller` row contains the preferred machine name, a monotonic revision,
and `heartbeat_at`. Every ordinary API process runs the same fixed five-second
poll. The named machine refreshes the timestamp while it passes the visibility
brake. A follower may replace an appointment after its heartbeat is more than
30 seconds old; the write compares both the exact observed revision and machine.
The first visible machine may similarly create revision one when the row is
absent. Corrosion LWW resolves concurrent writes.

The timestamp is weak wall-clock evidence, not a term, lease, expiry guarantee,
or fencing token. Forwarding failures return unavailable and never elect; this
leaves the single polling loop as the only appointment-change path. In
particular, an API listener failure is not detected while the same process can
still refresh Corrosion. That rare failure waits for process restart or operator
intervention.

The appointment is advisory. Work is admitted against the exact machine and
revision currently visible. A deploy rechecks that identity before
dispatching more host work and once immediately before committing cluster rows.
The commit itself is an ordinary unconditional Corrosion transaction. This
narrows the everyday race but is not fencing: a stale or partitioned controller
commit may still be accepted, and the next attempt repairs from current rows and
host reality.

## Controller execution

One deploy attempt is a plain function, as amended by ADR 0043:

1. read roster/config intent and inspect target hosts over bounded HTTP;
2. compute one placement from the complete request and fresh host reality;
3. ask each target node to prepare its local runtime;
4. recheck the exact controller appointment, then replace the namespace's one
   complete serving-intent row and insert missing automatic routes;
5. ask target nodes to retire exact obsolete runtime identities;
6. publish the coarse terminal Operation result.

The controller does not persist a plan, step machine, or recovery journal.
Stable operation, service, and replica identities plus fresh
inspection make retrying from reality safe enough for the small-cluster product.

## Node-local durable execution

Every node runs its own Duroxide runtime backed by a private local SQLite
database. It is used only for effects on that node:

- prepare resolves and pulls the image, then creates, starts, and checks the
  exact container;
- retire removes the exact obsolete runtime identity.

Completed prepare or retire activities are recorded so the same node can resume
after its daemon restarts. Each activity rechecks Docker reality rather than
durably journaling its internal calls. A single local worker serializes host
mutations. Read-only inspection bypasses Duroxide.

The databases are not a distributed workflow system. They do not elect the
controller, order cluster mutations, store cluster truth, or move between
machines. A host effect admitted under an old appointment may finish after the
appointment changes. Its controller may also win a later unconditional cluster
commit. Both races are accepted; a later attempt observes rows and hosts and
reconciles from that reality.

## Operation rows

Corrosion exposes only two deploy snapshots: created and terminal. Terminal
outcomes are completed, failed, or interrupted. There are no running snapshots,
step events, heartbeats, worker claims, ownership takeover, or replay journal.

An executing deploy that observes a foreign Controller Appointment may write
an interrupted terminal result. A controller crash can instead leave a created
row behind; no other controller projects, resumes, or rewrites it. Operation
rows are evidence, not a recovery queue. A caller retries with a fresh deploy
name from Corrosion and host reality rather than invoking a resubmission protocol or consulting
operation or Duroxide history.

## Partition contract

Partitions may create competing preferred controllers. This is accepted. We do
not add quorum or fencing to disguise it.

The only cluster-wide brake blocks an isolated member:

- one- and two-machine rosters may operate after the local roster query
  succeeds;
- a roster with three or more machines requires Corrosion health to report at
  least one other visible member.

This is deliberately not majority quorum. Equal partitions may both operate.
Immediate appointment rechecks reduce stale commits after convergence but
cannot make partitioned execution exclusive or reject a commit atomically.
Concurrent namespace or route writes may therefore both report success. They
target the same canonical name, so Corrosion converges the competing whole
documents at one row key; the losing intent may disappear without a retained
shadow. A later command observes that converged document and reconciles from
reality.

Named-volume support is intentionally one-shot. A namespace may receive its
first volume-bearing service deploy, but a later volume-bearing deploy is
refused synchronously while a different generation is present on an inspected node. Replicated volume services
are limited to one replica; global mode still means one independent local
volume per machine. A target also refuses a different deploy generation already
present in that namespace, and the controller refuses debris reported by any
responding machine, so a failed first attempt is not silently mounted beside a
retry. There is no holder discovery, affinity, handoff, migration, or
distributed volume fencing; an operator must remove its local runtime
explicitly before starting over.

## Failure and recovery

Controller loss abandons its in-memory attempt. A replacement takes a new
appointment after the old heartbeat is stale, and later caller retries observe
Corrosion and hosts. No controller history is recovered or migrated.

A deploy acceptance response may be returned before its first coarse Operation
row is written. Losing the controller in that window leaves no operation row;
the caller re-reads reality and retries. This small unknown-result window is
accepted instead of adding an outbox.

Node-local Duroxide history survives an ordinary daemon restart on that node,
subject to the guarantees of its local SQLite store. Workflow inputs may contain
registry credentials, so the database and its sidecars are private node state,
not operator evidence.

Duroxide 0.1.x is a preview dependency. Ployz uses its stock SQLite runtime
behind this deliberately narrow node-local boundary and does not extend it into
cluster coordination.

## Superseded guidance

This narrows ADR 0040's claim that v2 has no coordination point and that any
machine independently drives a mutation. Corrosion remains the replicated
store; there is still no replicated Core, quorum, sequencer, or NATS. Coarse
operation snapshots replace the former claims, state machines, and detailed
operation replay machinery.
