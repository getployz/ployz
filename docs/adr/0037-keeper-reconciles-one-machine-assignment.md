# Keeper Reconciles One Machine Assignment

Keeper is a connected machine-local daemon, and the machine assignment is the
only thing it reconciles. Workloads, routes, serving state, and every other
piece of cluster truth keep their operation owners; machine substrate is the
one layer where continuous reconciliation is correct, because a host that
cannot run its assigned processes cannot participate at all.

A machine assignment is the Control-compiled component set for one machine:
substrate components with their exact versions, plus typed host feature
entries such as pooled storage or reserved capacity. User-facing capability
profiles are presentation. Compiling a profile into components is core policy
and happens once, on Control, so fleet behavior does not vary with the keeper
version that received the assignment.

The reconciler's authority is exactly the assignment. Keeper converges on
process start, on assignment generation change, on explicit repair, and on a
periodic tick; it re-applies only what a recorded assignment already decided,
so every convergence is enforcement of an existing decision rather than a new
one. Quiet convergence emits nothing durable. Keeper may create and may stop.
It may never destroy data: pools, volumes, reserved capacity, and containers
are removed by explicit destructive operations, never by convergence.
Dropping a component stops its process and leaves its workloads as unresolved
machine cleanup, resolved through drain and graceful removal.

Changing an assignment is a validated intent write, not an operation. The
write is bounded, local, and atomic, so an operation record would describe
work that already succeeded or already failed synchronously. The assignment
carries its own provenance — generation, actor, and time — and Cloud owns
richer history. The rule that keeps this from eroding: bounded, local, and
atomic work is a write; work spanning hosts, processes, or time remains an
operation. Machine add, deploy, removal, and recovery stay operations.
`AGENTS.md` carries this carve-out.

Assignment generations are monotonic and fenced, so a machine that was offline
when its assignment changed converges on reconnect rather than blocking the
operator. Observed state is live testimony answered at the point of use.
Terminal convergence evidence is machine-local Local Authority: it records
what happened on that host, survives core loss, and is never copied into
cluster truth. There is no generic retry. There is converge again, which
re-runs enforcement and is always safe, and change the assignment, which is a
new decision.

Keeper's own version is one entry in the assignment it reconciles.
Convergence orders keeper first: stage, verify, restart, resume, then apply
the remaining components. A staged keeper that fails to come up reverts to the
previous binary, because losing remote management of a host is the one failure
this model cannot recover from. This supersedes ADR 0014; the separate keeper
update operation and its cross-operation version precondition are removed.

Claim authority stays narrow. Keeper mints its own keypair at install and
holds no authority until claimed, and it contacts a rendezvous only when the
claim material names one, so an installed keeper has no default network
behavior. Claim binds that public key to a machine identity, and Control mints
the least-privilege credential. Keeper reports host facts before any
assignment exists; core policy turns those facts into the available options
and defaults, and both the CLI and Cloud render that same option set rather
than each owning a question set. Control is the assignment authority with no
exceptions, so founder bootstrap forms a bare core first and then follows the
identical path as every other machine.
