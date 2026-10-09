# Ployz design

Ployz is a distributed deployment engine and the Config Store that holds what
it deploys. The CLI and the dashboard are equal surfaces over the same core
operations: every change to authored configuration goes through the Config
Store, which Cloud hosts for each Organization. A
Deploy Intent is derived from Saved State when a Deploy is admitted; there is no
second authoring format.

Ployz runs containerized services across a Cluster of user-owned Docker Machines
joined by a flat WireGuard mesh. There is no central control plane: every Machine
is an equal Entry Machine, state replicates between Machines as CRDTs, and a
Cluster Observation is what one Entry Machine sees — never a globally
authoritative entity.

Vocabulary: capitalized terms (Machine, Deploy, Cluster, …) carry the exact
meanings defined in [CONTEXT.md](CONTEXT.md).

## How to use this document

Judge every new feature against the bets below before designing it. Each bet states
the position, why Ployz holds it, and the red flags that signal a design fighting
it. A change that fights a bet needs a new bet here justifying the exception —
or a redesign. A red flag is not an automatic no; it is a demand for
that justification. The Boundaries section at the end lists what Ployz
deliberately does not provide; a feature that needs one of those is fighting the
design, not filling a gap.

## Stable promise

From 0.2.0: a daemon keeps working, and can be upgraded, across every later
0.x release without re-enrolling or reinstalling. Versions follow semver; only a
new major line (1.0 first) may break the promise, and it says so. The promise covers what a
daemon carries or speaks — the replicated store and its bodies, the local
Machine record, Machine RPC within `PROTOCOL_MAJOR`, the enrollment protocol,
the release source, the `/run/ployz/dns.json` spec a daemon hands its Internal
DNS process, and `ployzd dns --probe` exiting 0 when a binary can serve it. The CLI surface is a client courtesy with ordinary
deprecation, not a guarantee. Branch Sync broke it on purpose, with no users yet:
`env save` and `env update` are gone, and the JSON field `save` is now
`conditional_sync`.

Frozen formats evolve **additively with tolerant readers**: rows and bodies only
gain fields; every new field is optional with a default; nothing is renamed or
repurposed; readers ignore unknown fields. Replicated bodies never use
`deny_unknown_fields` — a newer Machine's row must remain readable by an older
one in the same Cluster. Authored config and unrelated trust boundaries stay
strict. Within frozen formats, explicit security refusals are the only
exception, and never on replicated bodies: a section holding secret or key material, or
a request mode that selects verification, may refuse fields it does not
recognize and fail closed. There are no version gates and no store migrations;
a change that cannot be expressed additively waits for `PROTOCOL_MAJOR` 2.

A breaking release protects older Clusters from its own side. Daemons follow
their own line's channel pointer (`ployz.sh/v0/stable`), so a new line never
reaches them unasked. The breaking release's CLI refuses to init or add a
Machine into a Cluster on another line, and its daemon rejects older-line
joiners. An older release never has to know about a newer one.

**Red flags:** a required new field, a migration step, a reader that rejects
unknown fields on replicated data, a daemon behaviour that depends on every
peer being upgraded.

## 1. Observer-relative truth

**The bet.** Every view of a Cluster is one Machine's observation at a point in
time. No component is entitled to declare authoritative cluster state, and none
exists.

**Why.** Without a central control plane, "the" cluster state would require
consensus we refuse to pay for. Commands act on an observer's snapshot and report
what that observer saw; different observers may legitimately disagree until their
observations converge. Weak semantics stated honestly beat strong semantics
enforced badly.

**Machine-local lease records.** A Volume switch is a Cloud-driven sequence of
Machine RPCs, and Cloud's own late or retried requests can arrive after the
Machine has moved on. Each Machine keeps one lease record per Volume on its Pool
and fences such requests against it: a stale lease or an earlier step is refused,
a repeat answers what it already did. That record is admission, not
coordination. The Machines decide it, Cloud carries it, and no Machine's record
claims to be the Cluster's view of the Volume.

**Red flags:** fencing tokens, leases, leader election, quorum reads, any API or
message claiming a complete or canonical Cluster view. A Machine-local lease
record that fences Cloud's own late requests is none of these.

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
snapshot, execute, report, stop. No Service placement process runs forever.
Global Services are placed by Deploy and by bounded catch-up when a Machine joins.
Catch-up rejects partial Live Observations before planning, reads fresh target
storage evidence, reports unknown eligibility or
incomplete placement, and stops; it does not leave work for a background loop.
Deleted slots, later eligibility changes, and transient failures require an
explicit redeploy.

**Why.** Imperative errors surface predictably at the caller that can act on them.
Declarative reconciliation decouples components but multiplies edge cases and
hides failures behind "it will fix itself later." Docker restarts containers on a
Machine; Ployz does not move them between Machines on its own.

A Volume Run (Mirror, Sync, Delete Mirror) is the one durable workflow, and it is
still a bounded attempt. Cloud runs it as a fixed list of steps a user requested;
it ends done, failed, cancelled or lost, and never runs again on its own. A retry
reads every copy live and continues from what the disks show. A Cluster without
Cloud keeps its Volumes where they are.

**Red flags:** controllers, persisted desired state, durable workflows other than a
requested Volume Run, cluster-wide reconcilers, any behavior that continues after
its command returns.

## 4. Partial results are outcomes

**The bet.** A fan-out returns successes together with per-target failures and
omissions — an expected outcome, not a failed transaction. A Deploy may complete
only a prefix of its plan; there is no atomicity and no general rollback, only
narrow, explicit compensation. A failed Upgrade that already replaced the daemon
restores the prior release: Replacement Compensation, allowed only within one
release line, where the Stable promise keeps everything the newer daemon wrote
readable by the older one.

**Volume handover.** A Volume Move can restart its stopped source Container with
Thaw until HandOver records the handed marker. HandOver is the point of no
return: after it, the only exit is forward through target promotion and start.
Thaw and HandOver make that decision under the source's dataset lock. Close
keeps the read-only source as the reverse mirror before Docker forgets its
Volume registration. Moving back is another Move, with that mirror as its base.

**Why.** Pretending a multi-Machine operation is atomic requires either lying in
the result or coordination machinery bet 1 forbids. Reporting the true
prefix/suffix lets the operator or a retry act on facts.

**Red flags:** all-or-nothing semantics, automatic rollback, results that collapse
per-target detail into one boolean.

## 5. Two-tier identity

**The bet.** Entities (Machines, Containers) are entity-keyed: their creator mints
an opaque durable ID unilaterally, and Machine Name or Service Name collisions
coexist forever — Name Ambiguity is preserved, never repaired. Declared,
replicated facts (certificates, service groupings) are name-keyed:
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
possible across independent operators, never prevented by a mandatory global
allocation step. An operator serializes its own enrollment commands with durable
allocation history, including assignments not yet visible in an observation.
History is saved before publication and Join and retained after failures; it is
not runtime truth and has no automatic expiry or reclamation.

**Why.** A global allocator that must answer before a Machine can act is a
consistency dependency in disguise; it turns every partition into an outage.
Different computers, independent stores, and direct CLI and Cloud operators can
still select overlapping subnets from incomplete observations. No cross-store
synchronization or subnet repair system is provided.

**Red flags:** a resource identity meaningful without its Machine, a required
round trip to an allocator, refusing to operate because an allocator is
unreachable.

## 7. Cloud drives, never owns

**The bet.** Cloud hosts each Organization's Config Store and drives its Deploys,
whether a user works in the dashboard or the CLI, but it is not a Cluster
controller and holds no runtime truth. SSH and the Management Capability use the
same Machine RPC connection seam. The daemon serves Machine RPC in-process on its
Management Identity, an iroh key that is not a mesh peer; clients reach it through
the Ployz-hosted Ployz Relay, which sees ciphertext only, or a direct path.

**Why.** Reusing transport primitives removes a hosted protocol to maintain.
Cloud stores encrypted, Organization-scoped connection candidates associated with
the current Cluster pairing. A saved candidate is neither membership nor presence;
only a successful connection confirms reachability and the intended Machine.
The Cluster remains independently operable without Cloud.

Volume Runs need Cloud to start and to step. Each Machine fences them itself: it
keeps a lease record per Volume and refuses a request from an older run or an
earlier step. Without Cloud a Volume stays put. No Cluster operation's correctness
depends on Cloud, only the availability of Volume Runs.

Removing Cloud access disables that pairing's connections immediately. Endpoint
revocation is confirmed separately; an offline Machine remains unconfirmed.
Management Capabilities grant shared administrative access: rotation affects every
old holder, while Cloud logout does not revoke direct capabilities or SSH keys.

Cloud forgets a Cluster it can no longer reach only when a user says its Servers
were deleted, and only while none of them answers; a failed connection may gate
that decision, never make it.

**Red flags:** Cloud-held runtime truth, a connection catalog treated as membership,
absence or transport failure used to reset a founder, Cluster operations whose
correctness depends on Cloud reachability.

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
between them. Caddy is the only Ingress Proxy; its implementation is not a
Cluster setting.
A Container leaves every Ingress Proxy before it stops: the client marks it
stopping and waits until each proxy's loaded config drops it, instead of tuning
proxy retries to cover traffic sent to a Container that is already gone.

**Why.** Every primitive we own is a primitive we patch, secure, and debug
forever. The maintenance budget belongs to the coordination semantics above, which
nobody else will build.

**Red flags:** hand-rolled consensus, custom overlay networking, bespoke TLS,
reimplementing behavior a shipped, proven component already provides.

## 10. Client-first, daemon when forced

**The bet.** Behavior lands in the client first. The daemon owns only behavior
that must continue without a client, enforce a Machine-local safety boundary, or
manage a Machine-local resource — Machine lifecycle, networking, local Docker
operations and observations, Machine-local serving infrastructure. New daemon
policy needs one of those reasons.

**Why.** Client changes are cheaper to distribute than daemon changes: a daemon
change must reach every Machine in every Cluster, while a client update ships
instantly. This is an economic preference, not a rule that all coordination
belongs in the client.

**Machine-local admission.** Before creating a Service Container or hook, or
preparing Service storage, the daemon reassesses placement against fresh local
evidence and ensures mounted Volume readiness, including Provisioned Volumes.
New creation and storage preparation are refused when eligibility is ineligible
or unknown. Machine Labels and acceptance flags gate admission; editing them does
not evict existing Containers or withdraw traffic. Starting, restarting, stopping,
and removing existing Containers remain available. The next explicit Deploy or Drain applies
current eligibility before replacement, including Machine-local Volume locality.
Build admission checks Build acceptance again after queue wait; already-admitted
Builds may finish.
Observer-side eligibility remains advisory, including an Unknown safe hold. A
Global catch-up client reads fresh target evidence, creates and starts eligible
slots, retires definitely ineligible slots, and holds unknown slots unchanged.
These separate observation and lifecycle calls are not atomic; new creation and
storage preparation independently enforce fresh Machine-local admission.
These checks admit work for an already-selected target; they do not schedule
work across Machines.

**Volume switch verbs.** Moving a Provisioned Volume between Machines is ZFS
work on both Pools: snapshots, send and receive, promoting a mirror, fencing a
late request against the lease record. That is Machine-local resource management
and a Machine-local safety boundary, so the daemon owns these verbs and the
tasks behind them (a receive that outlives the request, a Promote, a Start),
each admitted on this Machine's own record and markers. Cloud sequences the
verbs; it never performs one.

The Container steps are such tasks too: Freeze's docker stop, and the docker
start of a Thaw or a handed Start. Docker decides how long they take, and a
caller's deadline cannot know that, so the step runs in a task keyed by the
Volume's lease record and the mount grant lives as long as the task. The verb
answers within a few seconds, with Busy while Docker works. A replay with the
same record waits on the task, or reads what Docker answered, and Docker runs
once per record. Like a receive, this is behavior that continues after its
command returns, which bet 3 flags. It is bounded: the task ends when Docker
answers, it never retries or starts other work, and the Volume Run that asked
keeps asking until it does.

**Serving outlives the daemon.** Corrosion and Internal DNS are daemon-owned but
not daemon-hosted: a daemon stop, restart, crash or upgrade never stops them. The
daemon derives what they run from the Machine record and converges them on every
start. Corrosion is a Docker container carrying a digest of everything it was
created from, replaced only when that digest changes. Internal DNS is `ployzd
dns` under a runtime unit the daemon writes to `/run/systemd/system`, serving the
spec the daemon publishes in `/run/ployz/dns.json` and parking its port-53
sockets in systemd's fd store, so its own restarts queue queries rather than
refuse them. The daemon restarts it onto new software only after its own
Corrosion is ready. Reset and uninstall are the only paths that stop either one.
If the installed binary cannot serve DNS (`ployzd dns --probe` fails), the
running DNS process hands port 53 back and stops. This is a process that
continues after the command that started it returns, which bet 3 flags. It is
serving, not reconciling: it reads one spec, answers queries, and does no work
on other Machines or on its own schedule.

**Red flags:** daemon-side policy without one of the three reasons, daemon logic
a client could compute from the observations it already gathers, a daemon stop
path that stops serving infrastructure, a second writer of `dns.json` or the DNS
unit, DNS answering before its first load.

## Boundaries

Ployz deliberately does not provide:

- an authoritative global Cluster view;
- a centralized scheduler or control-plane quorum;
- Cluster-wide atomic operations or general rollback;
- automatic correction of every difference between observations;
- a generic abstraction over container runtimes — Docker is the runtime;
- continuously replicated persistent storage. A point-in-time mirror of a
  Volume, one per Volume, refreshed on request, is in scope; continuous
  replication is not.

These boundaries keep failure visible and each Machine independently useful. Add
a stronger guarantee only when the product requires it and the system can prove
it.
