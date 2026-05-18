---
title: Slice 023 Deploy Candidate Cleanup ABI
status: completed
completed: 2026-05-18
plan: MVP/slice-023-deploy-candidate-cleanup-abi-plan.md
---

# Slice 023 Deploy Candidate Cleanup ABI

## What Shipped

Deploy now has an explicit pre-serving candidate cleanup ABI.

- Participants receive `cleanup_deploy_candidates` with typed deploy,
  instance, service, revision, and candidate-state fields.
- The coordinator tracks candidate state as `PrepareAttempted`, `Prepared`, or
  `Started`.
- A reversible pre-commit failure returns `DeployError::PreCommitFailed` with
  the original failure and a `PreCommitCleanupReport`.
- Cleanup failures name the failed node, candidate set, and structured cause so
  the operator-visible failure audience can see what still needs manual cleanup.
- Cleanup RPCs run with bounded concurrency.
- Recovery from a deploy decision with no serving commit cleans planned
  candidates without rerunning capacity, prepare, start, or serving commit.

No new durable candidate-cleanup fact was added. The existing deploy decision
fact plus absence of a serving commit remains the recovery boundary.

## Invariants Proven

- Old backends are not drained or stopped before a serving commit.
- Candidate cleanup is foreground, idempotent participant RPC, not hidden
  background reconciliation.
- No fake rollback is attempted after an irreversible/serving boundary.
- Cleanup-pending is operator-visible when a participant cannot be reached.
- Recovery is explicit and does not replay participant mutation.

## Metrics

Latest focused run of `deploy-candidate-cleanup-contract`:

```json
{
  "visible_nodes_at_decision": 2,
  "prepared_candidates": 1,
  "started_candidates": 1,
  "candidate_cleanup_requests": 4,
  "recovery_cleanup_requests": 2,
  "cleanup_pending_count": 1,
  "prepare_reruns_during_recovery": 0,
  "start_reruns_during_recovery": 0,
  "old_backend_drain_requests": 0,
  "old_backend_stop_requests": 0,
  "elapsed_ms": 48
}
```

## Simplification Notes

The simplify/review pass removed redundant recovery IDs derivable from the
manifest and changed cleanup from sequential per-node waits to bounded fanout.
Candidate vectors intentionally remain on cleanup failures because failure
audiences should not have to cross-reference another report field under stress.

The `PhasedCommand` trigger stays deferred. Deploy now has more explicit phase
bookkeeping, but the primitive should wait until at least three command families
repeat the same resume/phase/compensation structure.

## Verification

```bash
cargo test -p mvp-deploy --lib
cargo run -p mvp-e2e -- deploy-candidate-cleanup-contract
cargo run -p mvp-e2e -- deploy-commit-drain-contract
cargo run -p mvp-e2e -- deploy-restart-recovery-contract
cargo clippy -p mvp-deploy -p mvp-p2panda-transport -p mvp-p2panda-facts -p mvp-e2e --all-targets -- -D warnings
```
