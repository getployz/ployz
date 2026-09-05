# T02 — Serialize reset and container admission

Status: pending
Blocking dependencies: T01
Audit scope: F01

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployzd/src/machine/local_machine/container.rs; ployzd/src/machine/local_machine.rs; ployzd/src/machine/mod.rs; ployzd/src/global_reconcile.rs; ployzd/src/daemon.rs

## Work

Separate historical Machine access from mutation admission. Use one Machine-local async admission/reset guard held through creation/convergence or cleanup+phase commit. Reject Resetting/Uninitialized and require Participating for Global convergence; preserve current ordinary Joining-create policy. Ensure participation notifications cease Global work on reset. Analyze cancellation/lock order and already-admitted creates; retain recovery API availability after removal warning.

## Acceptance

A Resetting Machine kept alive after replicated-removal failure cannot create/recreate containers or Global slots. A concurrent admitted create cannot finish after reset cleanup without being included in cleanup. Historical inspection/network cleanup remains available; no deadlocks or cluster-wide locks.

## Verification

Lowest existing local Machine operation seam that controls async interleaving (Rung 1 or 2). A deterministic barrier/cancellation regression must exercise real admission/cleanup ordering, not just a phase helper.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T02.md`; coordinator owns index/status updates and final four-axis review.
