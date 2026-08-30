# Ployz design

Ployz runs containerized services across a Cluster of Docker Machines joined by a
flat WireGuard mesh. There is no central control plane: every Machine is an equal
entry point, state replicates between Machines as CRDTs, and a Cluster is what one
entry Machine observes — never a globally authoritative entity.

Vocabulary: capitalized terms (Machine, Deploy, Cluster, …) carry the exact
meanings defined in [CONTEXT.md](CONTEXT.md).

## How to use this document

Judge every new feature against the bets below before designing it. Each bet states
the position, why Ployz holds it, and the red flags that signal a design fighting
it. A change that fights a bet needs an ADR in `docs/adr/` justifying the
exception — or a redesign. A red flag is not an automatic no; it is a demand for
that justification.

## 1. Observer-relative truth

**The bet.** Every view of a Cluster is one Machine's observation at a point in
time. No component is entitled to declare authoritative cluster state, and none
exists.

**Why.** Without a central control plane, "the" cluster state would require
consensus we refuse to pay for. Commands act on an observer's snapshot and report
what that observer saw; different observers may legitimately disagree until their
observations converge. Weak semantics stated honestly beat strong semantics
enforced badly.

**Red flags:** fencing tokens, leases, leader election, quorum reads, any API or
message claiming a complete or canonical Cluster view.

## 2. AP over C — inside the Cluster

**The bet.** Availability and partition tolerance win over consistency. A
partitioned Cluster stays operable: each partition can be managed on its own, and
state converges eventually once the partition heals.

**Boundary.** Cloud-side organization state (enrollment, pairing) may be
consistency-first, because it lives in one hosted service rather than the mesh.
That exception stops at the Cloud boundary and never extends into the Cluster.

**Red flags:** a Cluster operation that blocks on quorum or consensus, treating a
partition as an error state rather than a working condition, importing Cloud-style
consistency into mesh behavior.

## 3. Bounded imperative commands

**The bet.** A Deploy is a bounded attempt: calculate against an observer-relative
snapshot, execute, report, stop. No cluster-wide process runs forever. The only
continuous convergence permitted is machine-local — a Machine converging its own
Global slots.

**Why.** Imperative errors surface predictably at the caller that can act on them.
Declarative reconciliation decouples components but multiplies edge cases and
hides failures behind "it will fix itself later." Docker restarts containers on a
Machine; Ployz does not move them between Machines on its own.

**Red flags:** controllers, persisted desired state, durable workflows,
cluster-wide reconcilers, any behavior that continues after its command returns.

## 4. Partial results are outcomes

**The bet.** A fan-out returns successes together with per-target failures and
omissions — an expected outcome, not a failed transaction. A Deploy may complete
only a prefix of its plan; there is no atomicity and no general rollback, only
narrow, explicit compensation.

**Why.** Pretending a multi-Machine operation is atomic requires either lying in
the result or coordination machinery bet 1 forbids. Reporting the true
prefix/suffix lets the operator or a retry act on facts.

**Red flags:** all-or-nothing semantics, automatic rollback, results that collapse
per-target detail into one boolean.

## 5. Two-tier identity

**The bet.** Entities (Machines, Containers) are entity-keyed: their creator mints
an opaque durable ID unilaterally, and Machine Name or Service Name collisions
coexist forever — Name Ambiguity is preserved, never repaired. Declared,
replicated facts (allocator role, certificates, service groupings) are name-keyed:
concurrent writes converge to one last-writer-wins winner, losing a merge silently
is expected, and the winner's generated ID endures as the handle that lineage and
history attach to.

**Why.** Without a global authority nobody can enforce unique names or arbitrate
concurrent creates, so ambiguity and merge loss are embraced rather than
half-prevented. Durable, unilaterally minted IDs make provenance (clones, lineage)
possible later without any new coordination machinery.

**Red flags:** an ID-minting authority or registry, fencing on merge loss,
assuming a name resolves to exactly one thing, using a name as an identity or an
ID as a grouping key, treating an ID's absence from one view as proof of
non-existence.

## 6. Machine-local resources

**The bet.** Volumes, subnets, and addresses belong to one Machine; their names
are meaningful only together with that Machine. Allocation is optimistic —
concurrent changes may produce overlapping Machine Subnets — and conflicts are
tolerated and repaired opportunistically, never prevented by a mandatory global
allocation step.

**Why.** A global allocator that must answer before a Machine can act is a
consistency dependency in disguise; it turns every partition into an outage. The
Allocator that does exist is itself only a Replicated Observation.

**Red flags:** a resource identity meaningful without its Machine, a required
round trip to an allocator, refusing to operate because an allocator is
unreachable.

## 7. Cloud is a byte pipe

**The bet.** The Cloud Relay is a hosted pipe Machines dial out to and hold open.
It carries opaque streams, interprets none of them, holds no Cluster observation,
and is not a Machine or mesh peer.

**Why.** A dumb relay keeps the hosted surface small and auditable. The Cluster is
fully functional without it, and no Cloud outage or compromise can corrupt Cluster
semantics — it can only sever the pipe.

**Red flags:** relay-side interpretation or routing of payloads, Cloud-held
Cluster state, features that work only through the relay.

## 8. Evidence over claims

**The bet.** Report exactly what was observed, completed, failed, and never
attempted. A diagnosis names only the earliest proven failure stage and may remain
Unknown; a 503 alone is not capacity evidence.

**Why.** In a system where every view is partial, confident guesses are lies with
good posture. Honest evidence lets a human or a retry make the next decision;
fabricated certainty makes it for them, wrongly.

**Red flags:** inferring certainty the observer lacks, error messages that guess
at causes, success indicators that hide unattempted work.

## 9. Lean on proven primitives

**The bet.** Ployz composes boring, battle-tested components — Docker for
containers, WireGuard for the mesh, SQLite-backed CRDT replication for state,
Caddy for ingress, systemd for lifecycle — and writes only the thin coordination
between them.

**Why.** Every primitive we own is a primitive we patch, secure, and debug
forever. The maintenance budget belongs to the coordination semantics above, which
nobody else will build.

**Red flags:** hand-rolled consensus, custom overlay networking, bespoke TLS,
reimplementing behavior a shipped, proven component already provides.
