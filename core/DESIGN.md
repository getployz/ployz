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
`conditional_sync`. Warm Move broke it on purpose too, with no users yet: switch
requests no longer carry `not_after_unix_seconds`, and AdoptLease is gone.
The Log Store release makes one approved exception while the user base is small:
`TailLogs` replaces `ContainerLogs` and `MachineLogs`, and `LogHistory` replaces
`ContainerLogHistory`. Clients and daemons need the matching log RPCs. The release
also removes `ForgetLogs`; deleting an Environment leaves its logs under the
normal retention limits. These two read RPCs are the log contract going forward.

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

The Log Store is outside the promise. It is disposable Machine-local state. A
daemon that finds a format version it does not know deletes it and starts
again, so the Log Store needs no migrations.

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

**Machine-local lease records.** A Volume switch is a sequence of Machine RPCs
driven by one operator, and that operator's own late or retried requests can
arrive after a Machine has moved on. Each Machine keeps one lease record per
Volume on its Pool and fences such requests against it. A stale lease or an
earlier step is refused, and a repeat answers what it already did. The operator
numbers the lease. The Machine only compares it with its own record (bet 6). No
Machine's record claims to be the Cluster's view of the Volume, and no Machine
reads another's.

**Red flags:** fencing tokens, leases, leader election, quorum reads, any API or
message claiming a complete or canonical Cluster view. A Machine-local lease
record that fences one operator's late requests is none of these.

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

A Volume Run (Mirror, Sync, Move, Release, Delete Mirror) is the one durable
workflow, and it is still a bounded attempt. An operator runs it as a fixed list
of steps a user requested. It ends done, failed, cancelled or lost, and never
runs again on its own. A retry reads every copy live and continues from what the
disks show. A Cluster with no operator to drive it keeps its Volumes where they
are.

The Log Store runs continuously, and it only observes the Machine it runs on.
It never places, moves, restarts, or removes a container, and no Deploy waits
on it.

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

## 6. Machine-local authority

**The bet.** Each Machine is authoritative over what is on itself, and over
nothing else. Volumes, subnets and addresses belong to one Machine, and their
names mean something only together with that Machine. A Machine decides from its
own Pool, its own Docker and its own records. It never decides by reading
another Machine's. Work that spans Machines, such as moving a Volume, is a
sequence driven by one operator. Cloud is that operator when the Cluster has it,
with its database as the operator's store. The operator orders its own steps.
Each Machine admits or refuses each step against its own facts. Ployz assumes
one operator drives a given Cluster at a time. It does not enforce that, and two
at once may collide.

**Why.** In an eventually consistent Cluster a Machine can know only itself for
certain. Anything it reads about another Machine is an observation that may
already be stale (bet 1). A global allocator, a Cluster-wide lease, or a barrier
that waits for every Machine is a consistency dependency in disguise, and it
turns every partition into an outage. A single writer inside the Cluster would
need the consensus bet 2 refuses. A single operator outside it is free: Cloud
already has Postgres, and a CLI has the person's own disk.

Allocation is optimistic. An operator serializes its own enrollment commands
with durable allocation history, including assignments not yet visible in an
observation. History is saved before publication and Join and retained after
failures. It is not runtime truth and has no automatic expiry or reclamation.
Different computers, independent stores, and direct CLI and Cloud operators can
still select overlapping subnets from incomplete observations. No cross-store
synchronization or subnet repair system is provided.

Volume Runs follow the same rule. The operator numbers each run's lease from its
own store and keeps one run open per Volume. Each Machine admits a step only if
it is not behind the Machine's own record (bet 1) and its own markers allow it.
If the operator's store forgets numbers, or two operators drive the same Volume,
a Machine refuses whichever request is behind its record. Neither side waits for
the other.

Cloud smooths what it can see, and promises nothing. A Machine that was removed
without a reset, while its Volume was restored elsewhere, may come back still
holding a writable copy and running its Container. That copy decides nothing on
its own. When Cloud next sees both copies, it will name the one with the older
lease as old and demote it. That demotion ships with Restore. Until then both
copies can take writes, and the old one's writes are lost when it is demoted.
That window is accepted and documented, not prevented.

A container's output is Machine-local too. It lives on the Machine that ran the
container and outlives it. That Machine deletes it once it passes the size or
age cap. Cloud reads it through the daemon and never copies it (bet 7).
Deleting an Environment does not delete its retained logs. Reusing its Namespace
and Service names can show the earlier Environment's output until retention removes
it. Deployment and Container selectors distinguish that output by their identities.

**Red flags:**

- A Machine deciding by reading another Machine's records. A replicated
  observation of other Machines may narrow what a Machine does, by refusing or
  naming a better command. It never authorizes an effect.
- A lease that must be above every Machine's record to be safe, so a run has to
  reach, fan out to, or wait for Machines it does not touch.
- A barrier across Machines, such as refusing to remove one Server while
  another's state is pending.
- A clock-based safety wait. That includes a deadline, a budget, or a grace
  period whose expiry is what makes an effect safe. A driver bounding its own
  patience is fine.
- A resource identity meaningful without its Machine, or a required round trip
  to an allocator.
- A Cluster-side single writer, or machinery that prevents two operators instead
  of documenting the risk.

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

Volume Runs need an operator to start and step them. With Cloud, that operator
is Cloud: its `volume_run` rows number each run's lease and keep one run open
per Volume. That is the operator's bookkeeping, like allocation history in bet
6. It serializes Cloud's own runs, and it is not runtime truth. Each Machine
fences every step itself. Cloud may read what Machines report to choose a lease
or to notice an old copy, but no Machine waits for Cloud's view. Without an
operator, a Volume stays put. No Cluster operation's correctness depends on
Cloud, only the availability of Volume Runs.

Removing Cloud access disables that pairing's connections immediately. Endpoint
revocation is confirmed separately; an offline Machine remains unconfirmed.
Management Capabilities grant shared administrative access: rotation affects every
old holder, while Cloud logout does not revoke direct capabilities or SSH keys.

Cloud forgets a Cluster it can no longer reach only when a user says its Servers
were deleted, and only while none of them answers; a failed connection may gate
that decision, never make it.

**Red flags:** Cloud-held runtime truth, a connection catalog treated as
membership, absence or transport failure used to reset a founder, Cluster
operations whose correctness depends on Cloud reachability, a Volume Run step
whose safety needs Cloud's rows to be the Cluster's truth.

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

The Log Store reads the files Docker's `local` log driver writes, a format
Docker does not document. The installer does not pin a Docker release, so CI runs
the reader's test against the latest one. The reader skips a frame it cannot
parse, records the skipped bytes as corrupt, and keeps reading.

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
- a single writer inside the Cluster. Sequences that need one take it from the
  one operator driving them, and two operators at once may collide;
- a generic abstraction over container runtimes — Docker is the runtime;
- continuously replicated persistent storage. A point-in-time mirror of a
  Volume, one per Volume, refreshed on request, is in scope; continuous
  replication is not.

These boundaries keep failure visible and each Machine independently useful. Add
a stronger guarantee only when the product requires it and the system can prove
it.
