---
title: Drain-Aware Deploys Need Target-Aware Local Mutation
date: 2026-05-10
category: docs/solutions/integration-issues/
module: deploy
problem_type: integration_issue
component: service_object
symptoms:
  - "Drain-aware redeploy E2E timed out during self-target machine drain over NATS RPC"
  - "Unchanged volume-backed services could stay pinned to machines marked Draining"
  - "ZFS volume moves from draining sources were blocked when send required an Active source"
root_cause: logic_error
resolution_type: code_fix
severity: high
related_components:
  - ployzd daemon request routing
  - NATS node RPC
  - deploy planner
  - real ZFS E2E scenarios
tags:
  - deploy
  - machine-drain
  - request-lanes
  - nats-rpc
  - volume-move
  - zfs
---

# Drain-Aware Deploys Need Target-Aware Local Mutation

## Problem

Normal deploys could avoid new work on draining machines while still retaining unchanged volume-backed work there. The real drain-aware redeploy scenario also exposed that `machine drain <self>` can be a local state mutation, not a remote RPC, so routing it through the same shared remote path can deadlock or time out the operation that is supposed to make the deploy safe.

## Symptoms

- `deploy preview` after `machine drain founder` could show no movement for an unchanged volume-backed service.
- Managed single-scope volume ownership stayed on the draining machine unless the operator hand-authored a move.
- The real ZFS scenario failed with a NATS RPC timeout while draining the local founder.
- ZFS send rejected the exact source needed for this flow when it required the source machine to be `Active` instead of allowing `Draining`.

## What Didn't Work

- Reusing passive slot retention policy as deploy-time behavior. Retaining an existing slot on a draining machine is reasonable for status views, but an invoked deploy should treat `Draining` as relocation pressure when an eligible replacement exists.
- Making movement only an explicit migration primitive. `migrate service` can render move hints, but a normal redeploy after durable drain intent still needs to infer the movement.
- Requiring ZFS move sources to be active. A draining source is still the current owner and must remain a valid final-transfer source.
- Routing every drain and standby request through the exclusive lane. That fixes self-target mutation, but makes remote drain/standby hold the daemon write lock across long NATS RPC and confirmation waits.

## Solution

Keep `Draining` as durable operator intent, read only when an operator invokes deploy. The planner turns that intent into explicit previewable deploy work:

- `crates/ployz-orchestrator/src/deploy/plan.rs` splits deploy-time retention from passive keep semantics with helpers such as `can_retain_existing_slot_for_deploy`, `should_move_volume_from_draining_source`, and `resolve_inferred_volume_move`.
- Unchanged replicated service slots on draining machines are replaced on active eligible targets during invoked deploys; passive retention semantics remain unchanged outside deploy planning.
- The planner synthesizes a `PlannedVolumeMove` for a single-scope managed volume on a draining machine when attached manifest services are present and an eligible storage-capable target exists.
- Attached services are pinned through the existing `service_volume_pin` path so the service and volume move together.
- `crates/ployzd/src/daemon/handlers/volume/zfs.rs` allows `Active | Draining` sources for ZFS send while still requiring an active target.

The daemon dispatch fix is target-aware rather than command-wide:

- `crates/ployzd/src/daemon/handlers/mod.rs` keeps the static lane for `MachineDrain` and `MachineStandby` as `Shared`.
- `request_lane_for_state` promotes only self-target drain/standby to `RequestLane::Exclusive`.
- Shared handling calls remote-only methods such as `handle_remote_machine_drain`; those reject accidental self-target routing.
- Exclusive handling calls local-capable methods such as `handle_machine_drain`, which bypass NATS for self-targets and calls `handle_machine_transition_self` directly.

## Why This Works

Deploy remains explicit. Marking a machine draining does not start a reconciler or silently rewrite cluster truth; the next deploy reads stored lifecycle intent, shows the inferred movement in preview, probes participants, executes ZFS transfer, and commits ownership at the relevant deploy phase commit boundary, defaulting to the final deploy commit for the default phase.

The request lane fix keeps the critical distinction between local mutation and remote coordination. A self-target drain needs mutable local daemon state under the exclusive lane. A remote drain is network I/O and confirmation waiting, so it must stay on the shared path and avoid holding the daemon write lock across RPC timeouts.

Volume movement and service placement also stay aligned. Because volume planning runs before service slot placement, a moved single-scope volume can pin attached services to the same target instead of producing a plan that moves storage to one machine and schedules the service elsewhere.

## Prevention

- Treat lifecycle state such as `Draining` as deploy-time input, not background reconciliation. Preview/apply should expose the resulting work before mutation.
- Keep planner and runtime validators aligned. If the planner can move from a draining source, the ZFS sender must allow a draining source while still rejecting invalid targets.
- For commands that can target either self or a peer, choose request lanes from current daemon state and the target identity, not only from the command enum.
- Keep remote-only handler variants for shared-lane paths so accidental self-target RPC routing fails close to the bug.
- Test both sides of mixed local/remote commands: the static lane, the state-aware lane, local mutation, and remote RPC behavior.
- Cover the full behavior with real E2E: drain the current volume owner, redeploy the same manifest, assert preview/apply reports movement, verify data on the target, and reapply to prove idempotence.

## Related Issues

- `docs/plans/2026-05-10-002-feat-machine-availability-aware-placement.md` is the implementation plan for this slice.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md` covers the broader control-plane rule: prove eligibility and participants before mutation.
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md` covers the adjacent status-surface rule: stored intent, durable status, and live observation are different facts.
