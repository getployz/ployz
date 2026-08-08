# A Preferred Controller Serializes Cluster Mutations

Ployz keeps Corrosion as the replicated cluster store and adds one small,
disposable coordination point. A singleton Corrosion row names the preferred
controller. Every machine still serves the API; a follower forwards cluster
mutations to the named machine over bounded HTTP.

The preferred controller is ordinary async Rust guarded by one in-memory
mutation lock. It observes current rows and target hosts, computes a plan, and
dispatches bounded effects. It has no durable queue or workflow history.
Overlapping mutations may be refused as `controller_busy` instead of waiting in
a persisted scheduler.

This is not a return to the Core. Corrosion remains the only replicated cluster
store. There is no consensus leader, quorum, sequencer, claim service, or NATS.

## Controller appointment

The `controller` row contains the preferred machine id and an opaque
appointment id. It has no term, lease, heartbeat, expiry, timestamp, or fencing
meaning.

The first API machine to handle a mutation may create the row. A follower may
replace it immediately after one hard connection failure to the current
preferred machine. Timeouts, HTTP responses, and protocol failures do not
trigger replacement. Corrosion LWW resolves concurrent replacements.

The appointment is advisory. Work is admitted against the exact machine and
opaque appointment id currently visible. A deploy rechecks that identity before
dispatching more host work and once immediately before committing cluster rows.
The commit itself is an ordinary unconditional Corrosion transaction. This
narrows the everyday race but is not fencing: a stale or partitioned controller
commit may still be accepted, and the next attempt repairs from current rows and
host reality.

## Controller execution

One deploy attempt is a plain function:

1. read Corrosion and inspect target hosts;
2. compute one placement from that fresh reality;
3. ask each target node to prepare its local runtime;
4. recheck the exact controller appointment, then publish the service,
   container, and optional automatic-route rows in one ordinary Corrosion
   transaction;
5. ask target nodes to retire exact obsolete runtime identities;
6. publish the coarse terminal Operation result.

The controller does not persist a plan, step machine, or recovery journal.
Stable operation, service, replica, and container identities plus fresh
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
rows are evidence, not a recovery queue. A caller retries from Corrosion and
host reality rather than invoking a resubmission protocol or consulting
operation or Duroxide history.

## Partition contract

Partitions may create competing preferred controllers. This is accepted. We do
not add quorum or fencing to disguise it.

The only cluster-wide brake blocks an isolated member:

- a one-machine roster may operate when that machine is visible;
- a roster with two or more machines requires the controller to see at least
  two Corrosion members.

This is deliberately not majority quorum. Equal partitions may both operate.
Immediate appointment rechecks reduce stale commits after convergence but
cannot make partitioned execution exclusive or reject a commit atomically.
Concurrent namespace or route writes may therefore both report success. Named
row readers select the lowest canonical ULID after convergence; other valid
rows remain `doctor`-visible shadows until explicit removal.

Volume-bearing deploys add a data-safety check at the effect boundary: every
accepted machine must answer fresh inspection, and each target serializes its
own mutations and refuses an unexpected active holder. This is not distributed
volume fencing.

## Failure and recovery

Controller loss abandons its in-memory attempt. A replacement takes a new
appointment and later caller retries observe Corrosion and hosts. No controller
history is recovered or migrated.

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
