---
title: Slice 017 Graceful Machine Remove
status: completed
completed: 2026-05-18
plan: MVP/slice-017-graceful-machine-remove-plan.md
---

# Slice 017 Graceful Machine Remove

## What Shipped

Slice 017 adds a small product-level graceful machine-remove command under
`MVP/machine` and proves it end to end with `machine-remove-contract`.

The command is intentionally built from existing primitives:

- membership intent is facts: `NodeRemovalStarted` then `NodeTombstoned`,
- route removal is the shared `ServingCommit` cutover primitive,
- destructive stop is gated by `ProjectionCatchUp`,
- peer removal is a consequence of tombstone projection and last-applied mesh
  snapshot reload,
- command results carry visible nodes, serving commit id, cleanup status, and
  fact keys.

## Implementation Notes

The new `mvp-machine` crate owns the graceful-remove command shape:

- `MachineRemoveCoordinator`
- `MachineRemoveRequest`
- `PendingMachineRemove`
- `MachineRemoveCommandResult`
- `MachineFactWriter`

The coordinator fails before mutation when the target is missing, already
tombstoned, already being removed, or unavailable on the prepare RPC subject.
After `NodeRemovalStarted` is written, failures become foreground command
failures or cleanup-pending results depending on whether serving cutover already
happened.

The E2E scenario uses docs-backed membership/removal facts and the current
bus-backed serving commit path through a harness-local combined `FactSource`.
That bridge is deliberately test-local. It lets Slice 017 prove graceful remove
without pretending deploy/serving restart durability has already moved fully to
docs-backed facts.

## Proof

Verified commands:

```text
just test
cargo test -p mvp-machine --lib
cargo test -p mvp-e2e
cargo run -p mvp-e2e -- machine-remove-contract
cargo run -p mvp-e2e -- membership-wireguard-contract
cargo run -p mvp-e2e -- deploy-commit-drain-contract
cargo clippy -p mvp-e2e -p mvp-machine --all-targets -- -D warnings
MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

`machine-remove-contract` asserts:

- four visible nodes survive into the command result,
- `NodeRemovalStarted` projects before route cutover,
- route cutover excludes the target from active backends,
- the target remains in old-backend drain metadata until cleanup,
- the target replies `NoNewWorkAndDrained`,
- stop is only accepted after projection catch-up evidence,
- tombstone is written after stop succeeds,
- projection rebuild from facts removes the target from live membership,
- the final WireGuard peer plan excludes the target,
- remaining source-to-destination mesh traffic still succeeds,
- sending to the removed target is rejected by the applied peer table.

Latest sample metrics:

```json
{
  "visible_nodes_at_decision": 4,
  "remove_duration_ms": 3,
  "route_commit_to_projection_ms": 6,
  "projection_rebuild_ms": 6,
  "tombstone_convergence_ms": 1,
  "wireguard_peer_plan_ms": 24,
  "remaining_traffic_success_count": 1,
  "removal_started_before_route_cutover": true
}
```

## Semantic Leverage

The business invariant is visible in the command and E2E:

```text
preflight -> probe -> removal intent -> prepare/drain ack -> serving cutover
-> projection catch-up -> stop -> tombstone -> peer-plan exclusion
```

That is the intended improvement over the old codebase shape. Machine remove
does not own transport, projection, serving snapshots, or WireGuard application;
it composes those primitives and keeps its own state small.

## Deferred

- Real runtime/container stop and workload transfer backends.
- Real host WireGuard interface mutation.
- Production iroh/PloyzBus join/remove RPC path.
- A final serving commit that removes old-backend drain metadata after cleanup,
  if product semantics require drain metadata to disappear immediately rather
  than age out through a later serving commit.
