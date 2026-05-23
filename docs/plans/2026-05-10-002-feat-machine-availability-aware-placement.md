---
status: completed
created: 2026-05-10
module: deploy
tags:
  - deploy
  - placement
  - migration
  - machine-lifecycle
---

# Machine Availability Aware Placement

## Problem Frame

Deploy planning already distinguishes new-placement eligibility from existing
slot retention, and `migrate service` can render explicit volume move hints.
The missing primitive is that a normal deploy does not yet treat a machine in
`Draining` as pressure to move work away. It can avoid new placements there,
but an unchanged service with a single-scope volume may remain pinned to the
draining machine until an operator hand-authors movement.

This slice makes deploy planning availability-aware without adding background
reconciliation. The cluster still does nothing until an operator invokes a
deploy or command. When a deploy is invoked, the planner can turn durable
machine lifecycle intent into explicit deploy work: move eligible single-scope
volumes off draining machines, place attached services on eligible targets,
and show the movement in preview/evidence before apply mutates anything.

## Scope

In scope:

- Treat `MachineLifecycle::Draining` as unavailable for retaining existing
  work during an invoked deploy when an eligible replacement exists.
- Automatically plan movement for single-scope managed volumes whose committed
  owner is draining and whose attached services are present in the manifest.
- Pin attached replicated services to the selected replacement machine.
- Preserve existing behavior that active home-data and compute machines are new
  placement targets, while standby and region-disabled machines are not.
- Keep all movement as deploy work: preview shows it, apply probes
  participants, ZFS transfer executes in the deploy, and ownership commits only
  at the deploy commit point.
- Add e2e coverage proving a normal deploy after `machine drain` moves a
  volume-backed service to an eligible peer.

Out of scope:

- Background self-healing or automatic deploys when a machine is marked
  draining.
- Moving global services with single-scope managed volumes.
- Low-disk policy, metrics scoring, or target selection preferences beyond the
  current deterministic eligible-machine order.
- Machine remove batching across all services.
- Cross-authority or cloud-dashboard automation.

## Requirements

1. Deploy preview/apply must not silently keep unchanged replicated service
   slots on draining machines when an eligible replacement target exists.
2. A single-scope managed volume on a draining machine must produce a planned
   volume move when its attached service is redeployed and an eligible
   storage-capable target exists.
3. Services attached to an automatically moved volume must be pinned to the
   target machine in the same deploy plan.
4. If no eligible target exists, planning must fail with a structured deploy
   error rather than falling back to the local machine or pretending the
   draining source is acceptable.
5. Preview and apply must use the same resolved plan semantics; apply still
   probes all participants before participant RPCs or commits.
6. The feature must remain deterministic so repeated preview/apply renders the
   same slot and volume target choices from the same stored state.
7. Real-ZFS e2e must prove the user-facing behavior: deploy service+volume on
   one node, mark it draining, redeploy the same manifest, and observe the
   service and data on the peer.

## Key Decisions

### Draining Is Deploy-Time Relocation Pressure

`Draining` is not a background trigger. It is durable operator intent that
deploy planning reads when the operator invokes deploy. This keeps the core
aligned with `VISION.md`: explicit commands, no reconcilers, and visible
preconditions.

### Automatic Moves Stay Inside Deploy Planning

The planner may synthesize `PlannedVolumeMove` from committed volume ownership
and machine lifecycle, but it must not write `DeployManifest` back to storage
or mutate service specs. The generated movement is deploy intent for this
operation only and becomes durable truth only through normal deploy commit
evidence.

### Storage-Capable Targets Only

Single-scope volume relocation must choose a machine that is both eligible for
new placement and storage-capable. Compute-only machines remain valid
stateless service targets, but they cannot receive a moved managed volume.

## Implementation Units

### U1. Add Draining Relocation Policy Helpers

Files:

- Modify: `crates/ployz-orchestrator/src/deploy/plan.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

Work:

- Add planner-local helpers for deciding whether committed work should move
  away from a machine during an invoked deploy.
- Keep `machine_policy::can_keep_existing_slot` as the retention helper for
  passive/live status surfaces; do not globally redefine it to mean "can stay
  forever".
- Update `desired_slots` tests so unchanged replicated slots on draining
  machines relocate to active eligible targets.

Test scenarios:

- Unchanged replicated service on active machine remains unchanged.
- Unchanged replicated service on draining machine is replaced on an active
  target when a target exists.
- Replicated planning fails when the only known machines are draining or
  standby.

### U2. Synthesize Single-Scope Volume Moves From Draining Sources

Files:

- Modify: `crates/ployz-orchestrator/src/deploy/plan.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

Work:

- When a committed single-scope volume is on a draining machine and has
  manifest-attached services, choose a deterministic eligible storage-capable
  target.
- Record this as `PlannedVolumeMove` so existing preview, phase planning,
  participant collection, and execution paths handle the movement.
- Pin attached services to the moved volume target through the existing
  `service_volume_pin` path.
- Reject unsupported cases using existing structured deploy errors where
  possible.

Test scenarios:

- Volume-backed replicated service on draining source produces one volume move
  and a replace slot on the target.
- The target must be storage-capable and new-placement eligible.
- A volume already on an active machine is not moved.
- Shared volumes are not auto-moved.

### U3. Add E2E Scenario For Drain-Aware Redeploy

Files:

- Modify or add: `crates/ployz-e2e/src/scenarios/*`
- Modify: `crates/ployz-e2e/src/scenarios/mod.rs`
- Modify: `crates/ployz-e2e/src/cli.rs`

Work:

- Add a real-ZFS scenario that deploys a service with a managed volume on the
  founder, mutates data, marks the founder draining, then applies the same
  manifest again.
- Assert preview/apply reports a volume move from founder to peer.
- Assert the target service reads the mutated data on peer, the ZFS dataset is
  present on peer, and the old service container is absent on founder.

Test scenarios:

- Normal deploy after drain moves service and volume.
- Reapplying after the move is idempotent or fails only with an expected
  structured no-op/error, never with data loss.

## Verification

- `cargo test -p ployz-orchestrator`
- `cargo test -p ployz-e2e`
- `just test`
- `just test-all`
- PR CI, including the new real-ZFS e2e scenario

## Risks

- Automatically synthesizing volume moves expands the planner's effect surface.
  Tests must prove preview and apply share the same plan and that movement
  still commits only at deploy commit.
- Draining has historically allowed unchanged slot retention. This slice must
  update tests intentionally and keep a clear distinction between passive
  retention policy and deploy-time relocation pressure.
- Real-ZFS e2e can expose long foreground RPCs. The NATS RPC policy fix from
  the migrate-service slice is a prerequisite for reliable CI.
