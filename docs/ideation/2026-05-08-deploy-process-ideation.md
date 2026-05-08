---
date: 2026-05-08
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
