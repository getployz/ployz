---
date: 2026-05-08
last_updated: 2026-05-09
topic: deploy-process
focus: future deploy process around volume migration, metrics-informed placement, deploy semantics, and namespace branching
mode: repo-grounded
---

# Ideation: Deploy Process

## Grounding Context

Ployz is an explicit-command small-cluster orchestration core. The product
direction in `VISION.md` names deploy, migrate, branch, promote, rollback, and
fork-volume as core primitives. The system should expose visible preconditions,
bounded effects, clear results, and verification hooks rather than background
controllers or reconciler loops.

Current deploy flow, as documented in `docs/routing-and-deploys.md`, is:
preview manifest, acquire one namespace deploy lease, probe eligible machines,
start candidate containers, append one immutable deploy commit, publish routing
events, then drain and remove old instances. The commit is the point of no
return. Before commit, failure aborts; after commit, cleanup failure is visible
state rather than deploy failure.

Current placement is intentionally simple. `crates/ployz-orchestrator/src/deploy/plan.rs`
and `crates/ployz-orchestrator/src/machine_policy.rs` keep existing slots where
possible, place new replicated slots across active machines, allow draining
machines to keep existing slots, and exclude standby machines from new
placement. Managed volumes are pinned to a machine, and services mounting a
managed volume are pinned to that volume's machine. Existing ZFS transfer
commands provide pieces for movement, but volume migration is not yet a
first-class deploy outcome.

`docs/authority-roadmap.md` separates stored intent, projections, live facts,
and health metrics. Metrics can inform a decision at command time, but must not
be promoted into durable truth or silently rewrite placement. Relevant past
learnings in `docs/solutions/` reinforce two guardrails: preflight final
participant sets before mutations, and keep truth separate from observations in
operator-facing surfaces.

External grounding: Nomad shows foreground placement with hard constraints,
soft scoring, and explicit promotion; Terraform shows reviewable plan/apply;
Kubernetes scheduler architecture is useful as filter/scorer prior art, while
controllers/deschedulers are warning examples for what Ployz should avoid by
default; ZFS send/receive/clone/promote/rollback supports state movement and
branching; Railway, Heroku, and Fly show environment branching and stateful
deploy constraints.

## Topic Axes

- Deploy lifecycle semantics
- Placement decisions
- Volume/state movement
- Namespace branching/promotion
- Evidence and status surfaces

## Ranked Ideas

### 1. Operation Ledger and Saved Deploy Plan

**Description:** A deploy plan becomes a reviewable operation object: inputs,
resolved participants, preconditions, placement rationale, volume actions,
rollback handle, expected verification, and expiry/fingerprint.

**Axis:** Deploy lifecycle semantics

**Basis:** `direct:` current deploy already has preview and an atomic commit
boundary; `external:` Terraform plan/apply; `reasoned:` humans and agents need
the same inspectable artifact before mutation.

**Rationale:** This turns deploy from a transient calculation into an explicit
contract that can be approved, revalidated, and applied without becoming a
standing desired state.

**Downsides:** Adds a new artifact lifecycle and revalidation semantics.

**Confidence:** 88%

**Complexity:** Medium

**Status:** Unexplored

### 2. Metrics-Informed Placement Scorecard

**Description:** Use metrics only at foreground planning time: hard filters
first, then soft scores with evidence such as CPU, memory, disk, volume
locality, region, RTT, and freshness. Persist the chosen placement and evidence
snapshot, not a standing policy.

**Axis:** Placement decisions

**Basis:** `direct:` metrics currently exist but placement does not consume
them, and `docs/authority-roadmap.md` says health metrics are observations;
`external:` Nomad placement scoring and Kubernetes filter/scorer architecture.

**Rationale:** This makes placement smarter without creating an autoscheduler or
background loop that rewrites durable truth.

**Downsides:** Requires careful stale/missing metric semantics and clear
fallbacks.

**Confidence:** 86%

**Complexity:** Medium

**Status:** Unexplored

### 3. Volume Migration as a Verified Handoff

**Description:** Model state movement as a typed protocol: snapshot, send,
receive, verify identity, mount, switch writer, keep source until confirmation,
then update durable volume ownership at the same operation boundary.

**Axis:** Volume/state movement

**Basis:** `direct:` volume records are machine-pinned today and ZFS transfer
commands exist; `external:` ZFS send/receive/clone/promote/rollback; `reasoned:`
state movement needs a visible writer-owner handoff rather than an implicit
copy-then-start procedure.

**Rationale:** This gives volume migration the same explicit, auditable shape as
deploys and promotions.

**Downsides:** Stateful cutover and rollback semantics are hard, especially with
live writes.

**Confidence:** 90%

**Complexity:** High

**Status:** Unexplored

### 4. Namespace Branch Record and Capsule

**Description:** Make branch namespaces first-class: parent namespace, base
deploy id, volume forks, secrets policy, routing identity, promotion target,
expiry, and provenance.

**Axis:** Namespace branching/promotion

**Basis:** `direct:` branch, promote, rollback, and fork-volume are named vision
primitives but namespaces are currently flat; `external:` Railway environments
and Heroku review apps.

**Rationale:** Branching becomes an operational primitive rather than a naming
convention layered over deploy and volume records.

**Downsides:** Introduces namespace lineage as durable product surface.

**Confidence:** 84%

**Complexity:** Medium

**Status:** Unexplored

### 5. Promote as an Atomic Namespace Traffic Switch

**Description:** A branch is prepared separately. `promote` validates readiness,
volume lineage, routing, rollback point, and then commits one promotion event.
Production deploy becomes prepare-then-switch rather than mutate-in-place by
default.

**Axis:** Namespace branching/promotion

**Basis:** `direct:` `VISION.md` names branch, promote, and rollback as core
primitives; `direct:` routing events are projections while deploy commits are
stored intent; `external:` Nomad canary promotion and Railway/Heroku
environment promotion patterns.

**Rationale:** This sounds like the safest long-term production deploy shape:
build confidence in an isolated namespace, then switch ownership/routing with a
bounded, auditable operation.

**Downsides:** It depends on clear branch lineage, volume lineage, routing
identity, and rollback semantics. It may be too heavy for simple stateless
deploys unless it remains one deploy mode among others.

**Confidence:** 87%

**Complexity:** High

**Status:** Explored

### 6. Operation Evidence Ledger

**Description:** A shared evidence/status surface for deploy, migrate, branch,
promote, rollback, and fork-volume: stored intent, lifecycle, live observations
used, health uncertainty, participant responses, and failure audience.

**Axis:** Evidence and status surfaces

**Basis:** `direct:` deploy status and deploy events already exist; `direct:`
project instructions require every failure to have an audience; `reasoned:`
primitives compound when their evidence format is shared.

**Rationale:** This gives humans, agents, and cloud operators a common way to
inspect what happened and choose the next command.

**Downsides:** Easy to overbuild unless tied to concrete operation lifecycles.

**Confidence:** 83%

**Complexity:** Medium

**Status:** Unexplored

### 7. Placement Without Metrics Mode

**Description:** Placement must still produce a clear deterministic answer, or a
clear precondition failure, when metrics are missing.

**Axis:** Placement decisions

**Basis:** `direct:` current placement is deterministic without metrics;
`reasoned:` metrics are live observations and can be stale, absent, or
untrusted.

**Rationale:** This keeps metrics helpful but never required for basic
correctness.

**Downsides:** Lower placement quality when metrics are unavailable.

**Confidence:** 91%

**Complexity:** Low

**Status:** Unexplored

## Rejection Summary

| # | Idea | Reason Rejected |
|---|------|-----------------|
| 1 | Stateful Deploys That Admit When Volume Movement Is Required | Duplicates stronger volume handoff and saved deploy plan ideas. |
| 2 | Preflight Participant Set For Stateful Deploys | Useful implementation variant of saved deploy plan, not distinct enough as a survivor. |
| 3 | Placement Explanation Report | Duplicates placement scorecard/evidence ledger. |
| 4 | Soft Placement Preferences Without Background Correction | Folded into metrics-informed scorecard. |
| 5 | Commit Boundary Status Timeline | Folded into operation ledger and evidence ledger. |
| 6 | Volume Fork Preview For Branch Environments | Folded into namespace branch record. |
| 7 | Negative Deploy Plan | Good detail for saved plan, not a standalone top idea. |
| 8 | Atomic Volume Move as Deploy Candidate | Duplicates verified volume handoff. |
| 9 | Rollback-First Volume Migration | Important detail for volume handoff, not separate enough. |
| 10 | Deploy Waybill | Useful metaphor; folded into operation ledger. |
| 11 | Runway Clearance Before Commit | Useful metaphor; folded into evidence ledger and saved plan. |
| 12 | Slotting-Aware Placement | Useful metaphor; folded into placement scorecard. |
| 13 | State Escort Convoy | Useful metaphor; folded into volume handoff. |
| 14 | Escrowed Promote | Useful metaphor; folded into atomic namespace promotion. |
| 15 | Surgical Timeout for Rollback Risk | Useful detail for confirmation UX; folded into saved plan/evidence ledger. |

## Storage Driver Update: 2026-05-09

### Grounding Context

Storage drivers look like a product-level capability tier, not a narrow
implementation detail. `VISION.md` already says ZFS is product strategy because
snapshotting, cloning, sending, and rollback make the core primitives cheap.
Current code follows that shape: daemon config has `[storage].zfs_root`,
`crates/ployz-runtime-backends/src/storage/mod.rs` exports only `ZfsDriver`,
the public volume API names ZFS in payloads, and deploy planning pins services
to the machine that owns their managed volume.

The current deploy planner has the right pressure points for a driver model.
`ResolvedPlan` already carries planned volumes, participants, service slots, and
a plan fingerprint. `VolumeIntent::Move` exists in the manifest vocabulary but
planning rejects deploy intent hints for now. ZFS transfer code already models
snapshot, optional incremental base verification, send, receive, GUID
verification, and visible transfer status. That is a strong prototype for a
driver-backed move protocol, but it should not remain ZFS-specific in the
planner or public operation shape.

External grounding supports a capability matrix rather than a fake lowest-common
denominator. OpenZFS exposes snapshot, clone, rollback, and send/receive.
Btrfs has subvolume snapshots and send/receive streams. Ceph RBD supports
snapshots, clone layering, and asynchronous mirroring with primary/non-primary
image roles. Docker volumes are persistent host-managed directories and Docker's
documented migration path is backup/restore-style copying through a helper
container. Those are meaningfully different promises.

The driver direction implied by the user:

| Driver | Intended role | Operational character |
| --- | --- | --- |
| Ceph-like HA driver | Endgame HA-capable storage | Volumes are not primarily machine-local; move is more like attach/promote/fence than copy. |
| ZFS driver | Default | Fast local snapshots, clones, rollback, and incremental transfer; strong primitive substrate. |
| Btrfs driver | Small-RAM tier | Similar local COW shape with send/receive, likely fewer assumptions and looser performance/quotas. |
| Plain Docker driver | Opt-out | Host-local persistence with explicit degraded semantics: stop-and-copy migration, no instant fork/rollback promise. |

### Storage Topic Axes

- Driver capability model
- Plan/apply semantics
- Move and machine removal
- Branch, promote, and rollback
- Placement and topology
- Operator/status surface

### Storage Driver Ranked Ideas

#### S1. Storage Capability Matrix as Plan Input

**Description:** Introduce a storage-driver capability report that deploy,
migrate, branch, promote, rollback, fork-volume, and machine-remove planning
consult before promising work. Capabilities should be explicit enums such as
`LocalSnapshot`, `IncrementalTransfer`, `CopyOnWriteClone`, `RemoteAttach`,
`SharedWritableVolume`, `CrashConsistentSnapshot`, `QuotaEnforcement`, and
`StopAndCopyOnly`. Plans then say "this operation is instant", "this operation
requires cutover downtime", or "this operation is unsupported on this driver"
before mutation.

**Axis:** Driver capability model

**Basis:** `direct:` current storage config and APIs expose ZFS directly;
`direct:` `VolumeIntent::Move` exists but is rejected by planning; `external:`
ZFS/Btrfs/Ceph/Docker have materially different snapshot, clone, transfer, and
migration semantics.

**Rationale:** This keeps Ployz honest. Storage drivers should not be hidden
behind a generic interface that makes Docker look like ZFS or ZFS look like
Ceph. The planner needs to know the promise before it writes an operation plan.

**Downsides:** Public capability vocabulary becomes product surface and has to
be versioned carefully.

**Confidence:** 93%

**Complexity:** Medium

**Status:** Unexplored

#### S2. Storage Actions in the Operation Plan, Driver Commands Below

**Description:** Add a driver-neutral `StorageAction` layer to saved deploy and
migration plans: create volume, snapshot, fork, warm copy, final delta, attach,
detach, promote writer, demote source, verify, retain source, and destroy
retained source. ZFS lowers those actions to dataset snapshot/send/receive.
Btrfs lowers them to subvolume snapshot/send/receive. Ceph lowers many moves to
image map/unmap, fencing, primary promotion, or mirror status checks. Docker
lowers movement to stop, archive/copy, restore, and verify.

**Axis:** Plan/apply semantics

**Basis:** `direct:` the May 8 deploy-process ideation already promoted
"Operation Ledger and Saved Deploy Plan"; `direct:` ZFS transfer currently has
stages and verification, but the operation payloads are ZFS-named.

**Rationale:** The primitive should be "move this volume/workload safely", not
"run `zfs send`". The plan can stay stable while each driver owns its executor.

**Downsides:** Requires a careful boundary between planner-visible semantics and
driver-private mechanics. Too much detail leaks backend internals; too little
detail hides safety-critical work.

**Confidence:** 91%

**Complexity:** High

**Status:** Unexplored

#### S3. Move Semantics Become Tiered by Downtime and Ownership

**Description:** Represent volume movement as one of several explicit movement
classes: `AttachElsewhere` for HA/shared drivers, `WarmTransferThenCutover` for
ZFS/Btrfs incremental send, and `StopAndCopy` for Docker opt-out storage. The
same `migrate` or deploy-intent move command can render different plans, but the
preview must show the class, downtime expectation, writer handoff, rollback
handle, and unsupported cases.

**Axis:** Move and machine removal

**Basis:** `direct:` current deploy planning pins services to managed-volume
machines and refuses unavailable volume owners; `direct:` ZFS transfer already
does warm-ish snapshot transfer with base GUID verification; `external:` Docker
documents backup/restore-style volume migration rather than snapshot lineage.

**Rationale:** This answers the user's "what would moving look like" question:
moving is not one operation internally. On Ceph it may be mostly placement and
writer fencing; on ZFS/Btrfs it is copy plus cutover; on Docker it is explicit
downtime.

**Downsides:** Users may find tiered behavior surprising unless previews and
errors are very plain.

**Confidence:** 94%

**Complexity:** Medium

**Status:** Unexplored

#### S4. Machine Remove Becomes a Storage Evacuation Plan

**Description:** `machine remove` should first classify each local volume by
driver and capability, then produce a storage evacuation plan before workload
drain. Ceph-like volumes may only need to prove alternate attach/writer
eligibility. ZFS/Btrfs volumes need target selection, warm transfer, final
delta, and ownership commit. Docker volumes either block removal unless the user
accepts stop-and-copy, or require an explicit `--accept-downtime` style intent
rendered into the plan.

**Axis:** Move and machine removal

**Basis:** `direct:` `VISION.md` names machine remove as "drain workloads off a
machine, transfer their state, take it out"; `direct:` project operations rules
say mutating control-plane operations fail fast when peers/preconditions are
missing.

**Rationale:** Machine removal is where storage-driver honesty matters most. A
machine with Docker-only volumes cannot be removed with the same promise as a
machine whose volumes are Ceph-backed or ZFS-incrementally transferable.

**Downsides:** Removal preview becomes larger, and the command needs good
defaults so the happy path still feels like one primitive.

**Confidence:** 89%

**Complexity:** High

**Status:** Unexplored

#### S5. Branch/Fork/Rollback Promise Classes

**Description:** Give branch, fork-volume, promote, and rollback explicit
promise classes derived from the driver: instant COW clone, snapshot-send clone,
HA image clone, full copy, or unsupported. ZFS and Btrfs can make local COW
branching cheap when source and target fit the same storage substrate. Ceph can
branch through RBD snapshots/clones with its own lifecycle and flattening costs.
Docker opt-out can still exist, but branch with persistent state becomes full
copy or unsupported rather than pretending to be instant.

**Axis:** Branch, promote, and rollback

**Basis:** `direct:` `VISION.md` names branch, rollback, and fork-volume as core
primitives and says ZFS is the substrate; `external:` Ceph RBD supports
snapshots and clone layering, while Docker's volume docs point to backup/restore
for migration.

**Rationale:** This keeps the first-screen primitive identical while making the
promise precise. `ployzctl branch prod` can still be the command; the preview
says whether state branching is instant, copied, degraded, or impossible.

**Downsides:** The public product will need vocabulary for "same command,
different storage promise" that does not feel like exposing backend trivia.

**Confidence:** 90%

**Complexity:** Medium

**Status:** Unexplored

#### S6. Placement Reads Storage Reach, Not Just Machine Eligibility

**Description:** Extend placement scoring/filtering with storage reach. For
local drivers, a service using a single-writer volume remains pinned to the
current owner unless the plan includes a move. For Ceph-like drivers, compute
machines can be eligible if they can attach the image and satisfy fencing,
client, network, and failure-domain preconditions. Btrfs/ZFS targets require
same-driver compatibility and transfer reachability. Docker targets require an
explicit copy plan.

**Axis:** Placement and topology

**Basis:** `direct:` current planner uses `service_volume_pin` and rejects
services whose volumes are on different machines; `direct:` current region
placement distinguishes home-data and compute eligibility but does not yet know
volume reach.

**Rationale:** Storage drivers affect "where can this run?" as much as CPU or
region does. A Ceph-backed database and a Docker-volume database should not
produce the same placement candidates.

**Downsides:** Planner complexity rises because placement can no longer be
computed independently from storage action planning.

**Confidence:** 88%

**Complexity:** High

**Status:** Unexplored

#### S7. Driver Evidence in Status and Commit Records

**Description:** Persist driver evidence at the operation boundary: driver kind,
capability class, source/target owner, snapshot or image identifiers, base
snapshot GUIDs/checksums where applicable, consistency barrier used, writer
handoff, retained rollback source, and verification result. Live progress stays
status/observation; committed ownership and lineage become durable deploy facts.

**Axis:** Operator/status surface

**Basis:** `direct:` existing ZFS transfer records track status, stage,
snapshot GUID, base snapshot GUID, bytes, and last error; `direct:` documented
solutions require truth/status/live observations to stay separated.

**Rationale:** Humans and agents need to know not only that a move finished, but
which storage promise it relied on and what can be rolled back. This also gives
cloud UI a clean way to explain degraded Docker behavior without private logic.

**Downsides:** Evidence schema can sprawl unless tied to concrete operation
lifecycles.

**Confidence:** 87%

**Complexity:** Medium

**Status:** Unexplored

### Storage Driver Rejection Summary

| # | Idea | Reason Rejected |
|---|------|-----------------|
| S-R1 | One generic `StorageDriver` trait with `snapshot`, `clone`, and `move` methods | Too low-level and likely to hide differences that must be visible in plans. Folded into capability matrix plus storage actions. |
| S-R2 | Make Ceph the default immediately | Endgame-aligned but too expensive and would undercut the current ZFS-first product strategy. |
| S-R3 | Treat Btrfs as exactly equivalent to ZFS | Unjustified. Btrfs is close enough for a local COW tier, but quotas, operational maturity, and transfer behavior need their own evidence. |
| S-R4 | Plain Docker volumes only for stateless services | Too restrictive. Docker opt-out is useful if Ployz is honest about degraded stateful operations. |
| S-R5 | Hide driver choice entirely from users | Conflicts with `VISION.md`: storage capabilities are visible product surface, not hidden generic abstraction. |
| S-R6 | Automatically move volumes away from unhealthy machines | Scope overrun. Metrics or health can suggest an operation, but state changes remain explicit commands. |
| S-R7 | Background replication loop for ZFS/Btrfs local drivers | Violates the project direction against silent reconciler loops rewriting cluster truth; can be a commanded DR/mirror primitive later. |
| S-R8 | Require every driver to implement branch/promote/rollback before shipping | Too expensive and blocks useful degraded tiers. Better to make unsupported/degraded promises explicit. |

### Storage Driver Sources

- OpenZFS documentation: `zfs send`/`receive` recreate snapshots on receiving
  systems and support incremental transfer between snapshots:
  <https://openzfs.org/wiki/Documentation/ZfsSend>
- Btrfs documentation: send/receive transfers subvolumes in streamable form:
  <https://btrfs.readthedocs.io/en/stable/Send-receive.html>
- Ceph documentation: RBD mirroring supports journal and snapshot modes, and
  RBD images have primary/non-primary roles:
  <https://docs.ceph.com/en/latest/rbd/rbd-mirroring/>
- Ceph documentation: RBD snapshots and layering support cloning block device
  images:
  <https://docs.ceph.com/en/quincy/rbd/rbd-snapshot/>
- Docker documentation: volumes are Docker-managed persistent stores, with
  backup/restore or migration shown through helper containers and archive
  copies:
  <https://docs.docker.com/engine/storage/volumes/>
