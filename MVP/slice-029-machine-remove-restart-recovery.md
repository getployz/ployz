---
title: Slice 029 Machine Remove Restart Recovery
status: completed
completed: 2026-05-18
plan: MVP/slice-029-machine-remove-restart-recovery-plan.md
---

# Slice 029 Machine Remove Restart Recovery

## What Shipped

Machine remove now has the same restart shape as deploy:

- `MachineRemoveDecision` records target, removal/tombstone epochs, reason,
  visible nodes, and the exact `ServingCommitPlan`.
- The decision is written after target probe and before removal-started,
  participant drain, or serving cutover.
- `MachineRemoveCleanupDone` is written only after stop succeeds and tombstone
  is durable.
- Recovery reads command facts plus the exact serving commit from any
  `FactSource`; projection state is not used as request context.
- Cleanup-done is accepted only if the expected tombstone fact also exists in
  the recovered fact source.

The proof also landed the shared p2panda store simplification documented in
[MVP/slice-029-shared-p2panda-fact-store.md](slice-029-shared-p2panda-fact-store.md).
Deploy, machine, routing, and the volume E2E fixture now reuse
`SharedPandaFactStore` instead of carrying local cloneable store shells.

## Proof

`machine-remove-contract` now exercises the crash point Slice 028 deferred:

```text
serving cutover committed -> coordinator/pending value dropped -> stop/tombstone not done yet
```

The scenario exports the surviving p2panda operations, imports them into a
fresh store through trusted replica authority, reconstructs pending cleanup from
facts, verifies probe/drain/serving writes were not replayed, waits for
`ProjectionCatchUp`, stops workloads, writes tombstone plus cleanup-done, and
then recovers a second time without RPC.

Latest sample metrics:

```json
{
  "visible_nodes_at_decision": 4,
  "coordinator_outage_ms": 7,
  "recovery_read_ms": 0,
  "projection_rebuild_ms": 4,
  "cleanup_done_recovered": true,
  "no_precommit_replay_after_recovery": true,
  "remaining_traffic_success_count": 1,
  "removed_peer_rejected": true
}
```

Verified during implementation:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-machine
cargo test --manifest-path MVP/Cargo.toml -p mvp-machine -p mvp-machine-p2panda
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts -p mvp-routing-p2panda -p mvp-deploy-p2panda -p mvp-machine-p2panda
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
```

## Semantic-Leverage Ledger

Before this slice, machine remove could only finish after serving cutover
because the E2E kept an in-memory `PendingMachineRemove`. That was not a real
coordinator restart proof.

After this slice:

- `mvp-machine` owns the command facts and recovery API.
- `mvp-machine-p2panda` owns only machine-specific error/outcome mapping.
- `mvp-p2panda-facts` owns repeated store locking, import/export, and
  `FactSource` delegation.
- The E2E reads as the product invariant rather than a storage workaround:
  probe, decision, removal-started, drain, serving commit, recover, projection
  proof, stop, tombstone, cleanup-done.

Current line counts:

- `MVP/machine/src/facts.rs`: 723 LOC.
- `MVP/machine/src/remove.rs`: 1,709 LOC.
- `MVP/machine-p2panda/src/lib.rs`: 810 LOC including tests.
- `MVP/e2e/src/machine_remove_contract.rs`: 1,187 LOC.
- `MVP/p2panda-facts/src/lib.rs`: 3,185 LOC.

Assessment: **green** on restart semantics, **yellow-green** on maintenance
surface. The command gained durable recovery and no-replay proof without a
generic workflow engine. Raw machine-remove LOC grew, but repeated p2panda
plumbing shrank across deploy, routing, machine, and volume fixture code.

## Deferred

- Automatic recovery on coordinator startup.
- Operator-facing `machine remove resume`.
- Production runtime/container cleanup backend.
- Kernel WireGuard apply backend.
- Cross-process p2panda-net machine-remove recovery beyond local operation
  replay.
- Generic `mvp-commands` / `PhasedCommand` until one more command repeats the
  phase/resume/compensation shape.
