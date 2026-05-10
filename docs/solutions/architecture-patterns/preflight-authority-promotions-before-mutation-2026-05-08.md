---
title: Preflight Authority Promotions Before Mutation
date: 2026-05-08
last_updated: 2026-05-08
category: docs/solutions/architecture-patterns/
module: storage authority promotion
problem_type: architecture_pattern
component: tooling
severity: high
applies_when:
  - Promoting machines into control-plane authority roles
  - Reconfiguring every peer in a replicated authority set
  - Persisting operator intent before restarting local runtime components
  - Routing mutating daemon RPCs to mixed or partially upgraded peers
tags:
  - authority
  - nats
  - storage-promotion
  - preflight
  - rollback
  - bootstrap-peers
  - region-role
---

# Preflight Authority Promotions Before Mutation

## Context

The NATS storage promotion slice added `ployzd machine storage promote` to move
active storage candidates into the replicated authority set. Review found that
the first implementation handled the happy path but left the protocol too
optimistic for real authority changes: duplicate or invalid targets could be
partially processed, old authorities were not always reconfigured, local
bootstrap peers were not written before restart, and remote peers were mutated
without first proving that every final authority daemon understood the new
command.

The follow-up code-review pass found the same invariant at a wider boundary:
persisted intent, compatibility, and placement eligibility have to be proven
before mutation. Inferring them afterward leaves the operator with partial
cluster truth and a weak recovery story.

The durable pattern is to treat control-plane promotions as a small transaction:
validate the final authority set, prove all remote participants are compatible,
persist local intent and bootstrap inputs, mutate peers only after preflight
passes, and roll back local files if the remote mutation does not complete.

## Guidance

Use one transaction shape for authority promotion:

- Build and validate the final authority set before any mutation.
- Preflight every remote final authority, including existing authorities during
  R3 to R5 expansion.
- Persist local replica intent and all bootstrap peer records before mutating
  remote authority storage in the durable backend path.
- Mutate remote peers only after the compatibility and local persistence checks
  have passed.
- Restore local intent and bootstrap files if remote mutation fails before the
  operation reaches a consistent final authority set.

The founder needs the same final authority peer set as remote promoted machines,
otherwise restart can come up with stale single-node bootstrap inputs or
rollback to truth that no longer matches already-promoted peers.

Keep mutating RPC payloads narrow and validate them against current membership
before writing local config. `MachineStoragePromoteSelf` now carries
`MachineStorageAuthorityPeer` records instead of full membership records, and
the receiver checks peer count, duplicate IDs, local inclusion, endpoints,
public key, overlay IP, subnet, region role, lifecycle, and storage capability
before changing `storage_replicas` or bootstrap peers.

Make failures structured and audience-aware. `MachineStoragePromotionFailure`
now carries a `MachineStoragePromotionFailureCause`, so callers can branch on
`DuplicateTarget`, `InvalidCandidate`, `MachineNotFound`, `VersionMismatch`,
`RpcUnavailable`, or publish failures without parsing display text. Deploy
preview/apply uses the same rule for placement: a lack of eligible compute or
home-data targets returns `DeployFailureReason::NoEligiblePlacementTargets`.

Carry replica intent into every helper that can create or reconcile NATS assets.
Any daemon path that calls `NatsStore::start()` must first apply
`with_asset_policy(config.storage_replicas)`, including setup, deploy, and
node-RPC helper clients. Otherwise an R3/R5 authority can be silently
reconciled back to single-replica streams by an auxiliary client.

The same invariant applies to placement, but keep it scoped: machines in active
`home_data` and `compute` regions may receive new work. Draining regions may
retain unchanged slots for passive/status views, but invoked deploy planning
treats `Draining` as relocation pressure when an eligible target exists;
replacement work must move to an eligible target or fail with a structured
placement reason.

## Why This Matters

Authority promotion changes who owns durable control-plane state. A partial
mutation can create peers that believe they are replicated authorities while the
founder still restarts as a single-node store, or existing authorities that keep
the old replica policy while new peers use the new one.

Compatibility checks are part of the protocol, not a convenience. A missing
handler on an older daemon is a preflight failure; discovering it after one peer
has already changed roles leaves the operator with an avoidable recovery
problem.

Rollback also has to cover the local files that feed restart. Restoring only the
replica intent is incomplete if bootstrap peer records were already rewritten.

The same principle explains why missing persisted `storage_replicas` or
`region_role` should be rejected instead of defaulted. Constructors may choose a
local default, but loaded cluster truth should not invent authority or placement
intent that an operator never recorded.

## When to Apply

- A command changes authority, coordination, placement, or storage participation.
- The final set of participants is more important than the requested delta.
- Existing peers need to receive the new configuration as well as new members.
- A daemon-to-daemon RPC is newly introduced and mixed-version peers may exist.
- Restart depends on local files generated during the operation.
- A helper client can create, update, or reconcile durable backend assets.

## Examples

- The storage promotion handler sends `MachineStoragePromoteSelf` to every
  non-local final authority after status capability preflight, so replica policy
  changes reach existing authorities and new targets together.
- The promote-self handler validates `MachineStorageAuthorityPeer`, persists
  `storage_participation`, `storage_replicas`, and bootstrap peers before
  restart, restores previous config on failure, and returns typed failure
  causes.
- Setup, deploy, and node-RPC helper clients apply
  `with_asset_policy(config.storage_replicas)` before starting NATS stores, so
  auxiliary clients preserve the selected R1/R3/R5 policy.
- Deploy planning applies the same principle at the placement boundary:
  passive/status views may keep unchanged draining-region work, but invoked
  deploy planning treats `Draining` as relocation pressure when an eligible
  target exists; replacement placement must use an eligible machine or return
  `NoEligiblePlacementTargets`.

## Related

- `docs/authority-roadmap.md` tracks storage authority promotion as a stepping
  stone toward multi-authority NATS state.
- `docs/routing-and-deploys.md` applies the same intent/status/observation
  split to deploy planning and placement failure reporting.
- `docs/plans/2026-05-08-003-feat-nats-storage-promotion-slice-plan.md`
  describes the slice that introduced this command.
- `docs/plans/2026-05-08-004-feat-compute-only-region-placement-plan.md`
  describes the placement slice that reused the same authority-roadmap
  invariants for compute and draining regions.
- `docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md`
  covers the follow-up drain-aware deploy behavior for volume-backed services,
  ZFS movement from draining sources, and self-target drain request routing.
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`
  covers the adjacent status-surface rule: separate durable truth from live
  observation.
