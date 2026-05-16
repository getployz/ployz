---
date: 2026-05-16
topic: authority-local-hard-fork
focus: hard-fork control-plane architecture that deletes most existing control-plane code by replacing substrate features with authority-local files, direct RPC, live probes, and adoption
mode: repo-grounded
status: open
---

# Ideation: Authority-Local Hard Fork

## Grounding Context

Ployz is an explicit-command orchestration core for small clusters. `VISION.md`
is the product source of truth: no controllers, no autoscalers, no hidden
desired-state convergence, no silent background mutation of cluster truth, and a
disposable `ployzd` that adopts an already-running data plane on restart. The
operator loop is see the system, run a command, inspect the result, and decide
the next command.

`docs/authority-roadmap.md` already names the right separation: stored intent,
projection, live facts, and health metrics. It also says authority is ownership,
not geography; remote mutations fail now rather than queue; no automatic
failover rewrites authority ownership; and NATS is a mechanism rather than the
product model. The hard-fork direction should treat these rules as core
invariants, not as comments over the current NATS asset shape.

The current branch creates useful leverage but also shows where the old shape is
heavy. The workspace is now split across domain crates such as
`ployz-orchestrator`, `ployz-model`, `ployz-node-api`, `ployz-node-runtime`,
`ployz-store-api`, `ployz-nats`, runtime crates, build/image crates, and
supervision. `ployz-node-runtime` and `ployz-supervision` are already moving
long-lived local component lifetimes, shutdown, and health out of daemon
handlers. At the same time, the control-plane substrate is still expressed as
many authority-scoped NATS streams/KV buckets, a broad store facade, and broad
peer RPC surfaces that can blur ownership.

The current violations carried into this run are load-bearing:

- `cp_instances_<authority>` and `cp_image_availability_<authority>` behave like
  stored intent even though they are per-node observations.
- ACME challenge readiness is short-lived live state, not durable authority
  truth.
- Internal node RPC must not deserialize public daemon API requests.
- `cert_jobs_stream` is called a projection, but it is not mechanically
  rebuildable from durable certificate facts.

Relevant institutional learnings reinforce the same direction:

- `authority-status-separates-truth-from-observation-2026-05-08.md`: durable
  authority posture must come from stored records, while live probe failures
  attach uncertainty to known objects.
- `preflight-authority-promotions-before-mutation-2026-05-08.md`: validate the
  final participant set and peer compatibility before mutation.
- `extract-feature-workflows-behind-daemon-adapters-2026-05-13.md`: feature
  workflows belong behind narrow domain ports; `ployzd` adapts transport,
  timeout policy, and process wiring.
- `drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md`: durable
  lifecycle intent should be consumed by explicit invoked work, not background
  reconciliation.

External grounding supports a small set of borrowed ideas:

- Raft and Multi-Paxos are useful behind an optional quorum-backed authority log,
  not as the default relationship between all nodes.
- SWIM/memberlist/Chitchat-style gossip is useful for weak membership and live
  observation, but it is not durable authority.
- Erlang/OTP supervision gives a clean failure model for bounded command actors.
- Orleans and Cloudflare Durable Objects show addressable actors that activate
  on demand and pair execution with local state.
- Git/Merkle logs are useful for immutable history, sync, and integrity checks,
  but they do not choose the winner when two histories conflict.
- Dynamo-style eventual writes are a warning: high availability by accepting
  divergent writes pushes conflict resolution into reads, which is wrong for
  exclusive deployment intent.

## Deletion Thesis

The goal is not to preserve the current architecture with a better substrate.
The goal is to choose a new architecture that makes most of the existing
control-plane code unnecessary.

The deletion target is aggressive:

- Delete the NATS-shaped control-plane store as product architecture.
- Delete the broad store facade and most per-feature store traits.
- Delete durable buckets for live observations.
- Delete routing/job/status streams that cannot rebuild from authority facts.
- Delete broad `NodeRequest`-style peer RPC in favor of tiny local protocols.
- Delete background cleanup/reconciliation loops.
- Delete daemon-owned feature state.
- Delete feature-specific status tables when status can derive from command
  transcripts plus live probes.

The replacement should be smaller, not merely cleaner:

- One authority directory on disk.
- One append-only fact log per authority.
- One command transcript format.
- One direct peer RPC pattern.
- One live observation shape.
- One boot adoption classifier.
- One operator failure inbox.

If an idea does not delete substantial code or collapse multiple mechanisms
into one primitive, it is not strong enough for this hard fork.

## Topic Axes

- Authority truth and durability
- Command actor execution model
- Live observation and gossip
- Boot/rejoin adoption and drift cleanup
- Failure/status surface

## Ranked Ideas

### 1. Delete The Control-Plane Store Into An Authority Capsule Log

**Description:** Each authority owns one append-only capsule: typed fact
segments, checkpoints, rebuildable indexes, and disposable projections. The base
implementation can be plain files with fsync and hash-linked segments; R=3/R=5
quorum replication sits behind the same append contract only when the authority
explicitly opts into HA. NATS, if retained at all, becomes transport or an
optional replicated-log backend, not the product's core data model.

**Axis:** Authority truth and durability

**Basis:** `direct:` `docs/authority-roadmap.md` says an authority owns durable
control-plane truth and lists the current spread of NATS assets. `direct:`
`VISION.md` says durable state records operator intent and explicit lifecycle
events, while liveness is observed live. `external:` Git/Merkle-style immutable
history supports audit and sync, while Raft supports optional replicated
sequencing when one-node durability is insufficient.

**Deletion leverage:** This should delete the NATS asset map as architecture,
most authority-scoped KV/stream code, much of `StoreDriver`, fake projection
durability, and per-feature stored-status machinery. What remains is append,
fold, snapshot, and tail.

**Rationale:** This is the core simplification. It replaces many buckets and
store traits with one durable primitive: append authority fact, fold authority
view, rebuild projection. It also lets R=1 be honest, cheap, and normal without
blocking a future quorum-backed authority.

**Downsides:** A custom log format is still a database in the literal sense:
durable bytes, fsync rules, segment recovery, corruption handling, snapshots,
schema evolution, and tooling all become Ployz-owned responsibilities.

**Confidence:** 92%

**Complexity:** High

**Status:** Unexplored

### 2. Delete Orchestration State Into Proof-Carrying Command Actors

**Description:** Every mutating primitive runs as a short-lived actor keyed by
authority and command id. The actor writes a typed transcript: request,
preflight graph, live observations used, peer RPC attempts, point-of-no-return
fact append, verification evidence, cleanup debt, terminal outcome, and retry
safety. The actor exits after completion; the transcript is the durable audit
trail, idempotency record, status source, and resume input.

**Axis:** Command actor execution model

**Basis:** `direct:` `VISION.md` requires visible preconditions, bounded effects,
typed failures, idempotent operations, and verification hooks. `direct:`
`docs/architecture/node-runtime.md` forbids hidden reconciliation loops and
puts long-lived component ownership in runtime/supervision, not feature policy.
`external:` Erlang/OTP supervision shows how workers can fail visibly under a
supervisor without becoming autonomous policy engines.

**Deletion leverage:** This should delete daemon-owned feature state, bespoke
operation status tables, many ad hoc retry paths, and feature-specific
half-applied state machines. The command transcript becomes the common shape for
deploy, branch, cert, volume, machine, and adoption work.

**Rationale:** This turns "the control plane is summoned, commits, and
vanishes" into an implementation pattern. Deploy, machine add/remove, cert
issuance, volume movement, branch, promote, rollback, and adoption all get one
execution skeleton instead of bespoke partial-state handling.

**Downsides:** It forces every operation to define its transcript shape and
commit boundary. That is good architecture, but it makes vague operations
impossible to ship without first naming their states.

**Confidence:** 94%

**Complexity:** Medium

**Status:** Unexplored

### 3. Delete Global Peer RPC Into Capability Protocols

**Description:** Replace broad public-request-shaped node RPC with subsystem
protocol families exposed by node capabilities. A command actor calls verbs such
as `ProbeRuntime`, `PrepareContainer`, `VerifyVolume`, `InstallRoute`,
`AdoptArtifact`, `QuarantineArtifact`, or `FetchAuthorityFacts` through narrow
traits. Peers never receive `DaemonRequest`; they receive typed participant
commands with deadlines, version requirements, authority context, and explicit
allowed effects.

**Axis:** Command actor execution model

**Basis:** `direct:` AGENTS guardrails require internal node RPC to be a typed
protocol separate from public CLI/API requests. `direct:` the repo scan found
`NodeRequest` still acts like one large peer enum mixing mesh, machine, deploy,
volume, image, status, and storage promotion. `reasoned:` mostly-equal nodes
can be equal as protocol participants without being equal writers of durable
truth.

**Deletion leverage:** This should delete broad peer request routing, public API
forwarding to peers, daemon-shaped peer responses, and many unrelated handler
touch points when adding a feature. Each feature keeps only the remote verbs it
actually needs.

**Rationale:** This preserves the "every node is a peer" feel while making write
authority precise. Nodes can participate, observe, prepare, execute local
effects, and report evidence, but only the owning authority appends authority
truth.

**Downsides:** More protocol families mean more compatibility surfaces. The
benefit only holds if each protocol stays owned by its domain rather than
re-forming a different global enum.

**Confidence:** 90%

**Complexity:** Medium

**Status:** Unexplored

### 4. Delete Stored Live Facts Into Expiring Observation Postcards

**Description:** Live facts travel as signed, expiring observation postcards:
source node, subject, observation time, TTL, sequence, freshness class, and
payload. They can report reachability, runtime inventory, image availability,
volume presence, cert challenge readiness, disk pressure, peer RTT, and local
authority log head. Command actors may use them to narrow candidate sets, but
must verify important effects via direct typed RPC before mutation.

**Axis:** Live observation and gossip

**Basis:** `direct:` `docs/authority-roadmap.md` defines live facts and health
metrics as disposable and says not to promote them into stored truth.
`external:` SWIM/memberlist and Chitchat-style gossip are designed for weak
membership and observation dissemination, while external grounding warns that
gossip events are unordered, unpersisted, and not decision authority.

**Deletion leverage:** This should delete `cp_instances_<authority>`,
`cp_image_availability_<authority>`, durable ACME challenge readiness, and any
status path that writes observations as truth. Observation storage, if any, is a
cache with TTL and no authority semantics.

**Rationale:** This gives Ployz enough cluster texture to feel responsive
without turning freshness timestamps into policy. It also fixes the flagged
`cp_instances` and `cp_image_availability` mistake by making their shape
ephemeral by construction.

**Downsides:** Operators may still over-trust recent observations unless the UI
and API make freshness and uncertainty impossible to ignore. Placement code must
not quietly skip the direct verification step.

**Confidence:** 88%

**Complexity:** Medium

**Status:** Unexplored

### 5. Delete Restart Reconciliation Into Adoption Manifests

**Description:** Every managed data-plane artifact carries a small local
manifest: authority id, resource id, generation, config hash, creating command
id, ownership class, and disposal policy. On boot or rejoin, the node scans
Docker, WireGuard, sidecars, volumes, certs, gateway config, DNS config, and
unfinished command receipts, then submits an adoption report. The authority
returns an explicit plan: adopt, keep observed-only, quarantine, cleanup, or
require operator action.

**Axis:** Boot/rejoin adoption and drift cleanup

**Basis:** `direct:` `VISION.md` says `ployzd` is disposable and adopts what is
already running on startup. `direct:` current architecture docs define an
adopt-first lifecycle for data-plane services. `external:` filesystem journal
replay and `fsck` are strong analogies: replay committed records, inspect real
objects, quarantine or repair named drift.

**Deletion leverage:** This should delete scattered boot repair code,
feature-specific adoption paths, periodic cleanup loops, and stale-node special
cases. Boot becomes: replay authority facts, scan manifests, classify drift,
return an adoption plan.

**Rationale:** This is the restart/rejoin story that keeps simple code honest.
The system does not need special cases for "offline for hours," "daemon
restarted mid-operation," or "old container still running"; it classifies local
artifacts against authority facts and turns drift into visible work.

**Downsides:** Manifests must be attached consistently across runtimes and
survive local operator edits, Docker label loss, volume moves, and host-mode
services. Adoption can become complex if it tries to infer too much instead of
classifying conservatively.

**Confidence:** 91%

**Complexity:** High

**Status:** Unexplored

### 6. Delete Global Cluster Semantics Into Branchable Authority Islands

**Description:** Treat laptop authority, company compute authority, PR
environment authority, and future team authorities as independent islands with
their own capsule logs. Cross-authority work uses foreground typed handshakes:
export/import facts, signed/notarized receipts, lineage records, and explicit
failure when the target authority is unreachable. Branching and promotion become
authority-capsule operations composed with volume snapshot lineage and route
switch evidence.

**Axis:** Authority truth and durability

**Basis:** `direct:` the user wants authority islands where a laptop can request
access to deploy into company compute without becoming part of the same durable
truth system. `direct:` `docs/authority-roadmap.md` says multi-authority is an
explicit ownership split and remote writes do not queue. `direct:` prior
ideation on dev authority and PR workflow primitives already points toward
laptops and branch environments as real authority-shaped objects.

**Deletion leverage:** This should delete global cluster truth assumptions,
cross-authority queues, implicit shared membership, and topology code that tries
to make regions act like authorities. A laptop, PR environment, or company
compute pool is just an authority capsule plus handshakes.

**Rationale:** This is the product-defining reframing: scale-out is authority
composition, not one global cluster. It keeps a single server in `us-west`
perfectly valid, makes `nicks-laptop` a real local authority, and lets cloud/PR
workflows reuse the same primitive.

**Downsides:** The word "cluster" becomes less central, and docs/product copy
must be cleaned up to stop implying every node has equal durable state. Cross-
authority UX has to be strict enough that users understand which authority owns
which fact.

**Confidence:** 89%

**Complexity:** High

**Status:** Unexplored

### 7. Delete Stored Health Into A Failure Inbox And Next-Command Surface

**Description:** Each authority exposes an operator failure inbox derived from
durable command outcomes, adoption conflicts, preflight blockers, cleanup debt,
projection freshness loss, stale observations, and retry exhaustion. Each item
has an audience, resource, operation id, freshness, retryability, repairability,
last-good fact reference, and valid next commands. `ployz status` joins this
inbox with fresh live probes rather than reading one stored health field.

**Axis:** Failure/status surface

**Basis:** `direct:` project guidance says every failure needs an audience and
logs are not an audience. `direct:` `authority-status-separates-truth-from-
observation` says status must distinguish stored truth from live observation.
`reasoned:` a no-reconciler architecture remains usable only if unresolved work
is easy for humans and agents to see and act on.

**Deletion leverage:** This should delete stored aggregate health fields,
feature-specific failure tables, status inferred from stale observations, and
background jobs whose main purpose is to make status look current. Status
becomes facts plus probes plus next valid commands.

**Rationale:** This makes loud failure practical. It gives agents a native work
queue without adding autonomous self-healing, and it gives humans a status
surface that says "what happened, what is known now, and what command is valid
next."

**Downsides:** The inbox must avoid becoming a hidden scheduler. It should name
work and valid commands, not run them silently because an item exists.

**Confidence:** 93%

**Complexity:** Medium

**Status:** Unexplored

## Rejection Summary

| # | Idea | Reason Rejected |
|---|------|-----------------|
| 1 | Authority-Local Fact Log | Duplicate of the stronger Authority Capsule Log, which adds checkpoints, projections, and optional quorum backend. |
| 2 | Flat-File Authority Log | Duplicate of Authority Capsule Log; useful implementation constraint but too narrow as the top-level idea. |
| 3 | Git-Style Authority Reflog | Strong analogy folded into Authority Capsule Log. |
| 4 | Delete The Global Store Facade | Consequence of Authority Capsule Log, not a separate idea. |
| 5 | Invert Projection Ownership: Logs First, Views Disposable | Important rule folded into Authority Capsule Log. |
| 6 | Consensus Behind One Authority Door | Important implementation option folded into Authority Capsule Log. |
| 7 | One-Shot Command Actors | Duplicate of Proof-Carrying Command Actor. |
| 8 | Summon Command Actors, Then Kill Them | Duplicate of Proof-Carrying Command Actor. |
| 9 | Command-Summoned Brain | Duplicate of Proof-Carrying Command Actor. |
| 10 | Deployment Runs As A Ledgered Command Actor | Narrower form of Proof-Carrying Command Actor. |
| 11 | Airline Flight Release Command Actors | Good analogy, folded into Proof-Carrying Command Actor. |
| 12 | Erlang One-Shot Supervisors For Commands | Good execution detail, folded into Proof-Carrying Command Actor. |
| 13 | Preflight Graph Compiler | Critical command-phase detail, folded into Proof-Carrying Command Actor. |
| 14 | Typed Peer RPC As The Only Remote Write Path | Duplicate of Peer Capability RPC. |
| 15 | Peer RPC Families Instead Of Global `NodeRequest` | Concrete implementation shape folded into Peer Capability RPC. |
| 16 | Invert Node RPC Around Capabilities | Duplicate of Peer Capability RPC. |
| 17 | Typed Peer Capability RPC | Duplicate title variant of Peer Capability RPC. |
| 18 | Authority-Only Writes, Peer-Only RPC | Duplicate of Peer Capability RPC. |
| 19 | Membership Is A Capability Registry, Not Equality | Useful reframing folded into Peer Capability RPC and Branchable Authority Islands. |
| 20 | Notary Packets For Cross-Authority Facts | Strong cross-authority trust detail folded into Branchable Authority Islands and Peer Capability RPC. |
| 21 | Disposable Live-Facts Fabric | Duplicate of Expiring Observation Postcards. |
| 22 | Live Facts Are Never Stored As Truth | Rule folded into Expiring Observation Postcards. |
| 23 | Gossip Only Carries Hints, Never Decisions | Rule folded into Expiring Observation Postcards. |
| 24 | Make Live Observation Unpersistable By Type | Compile-time guardrail folded into Expiring Observation Postcards. |
| 25 | Live Observation Mesh With Expiring Hints | Duplicate of Expiring Observation Postcards. |
| 26 | Railway Token Blocks For Peer RPC | Interesting but heavier coordination metaphor; movement-specific tokening belongs in a later brainstorm on movement semantics. |
| 27 | Boot/Rejoin Adoption As An Authority Protocol | Duplicate of Adoption Manifests Everywhere. |
| 28 | Boot/Rejoin Is Adoption, Not Reconciliation | Rule folded into Adoption Manifests Everywhere. |
| 29 | Automate Adoption As A Boot Transaction | Duplicate of Adoption Manifests Everywhere. |
| 30 | Boot Adoption Court | Strong metaphor folded into Adoption Manifests Everywhere. |
| 31 | Passport Stamps For Boot And Rejoin | Good analogy folded into Adoption Manifests Everywhere. |
| 32 | Filesystem Journal Replay Plus Orphan Quarantine | Good algorithmic detail folded into Adoption Manifests Everywhere. |
| 33 | Replace Cleanup Loops With Explicit Drift Actors | Cleanup side of Adoption Manifests Everywhere. |
| 34 | Authority Islands, Not A Cluster | Strong reframing folded into Branchable Authority Islands. |
| 35 | Authority Islands With Explicit Handoff Records | Duplicate of Branchable Authority Islands. |
| 36 | Island-First Authorities | Duplicate of Branchable Authority Islands. |
| 37 | Resource Movement Ledger | Strong but better handled as a later brainstorm under command transcripts or branchable authority movement. |
| 38 | Operator Failure Inbox | Duplicate of Failure Inbox And Next-Command Surface. |
| 39 | Failure Inbox, Not Self-Healing | Duplicate of Failure Inbox And Next-Command Surface. |
| 40 | Status Is A Failure Ledger Plus Live Probe, Not A Health Field | Rule folded into Failure Inbox And Next-Command Surface. |
| 41 | Automate Status From Last Actor Outcome Plus Live Probe | Duplicate of Failure Inbox And Next-Command Surface. |
| 42 | Medical Triage Status Board | Good analogy folded into Failure Inbox And Next-Command Surface. |
