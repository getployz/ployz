# T08 — Reject known host socket conflicts during placement

Status: complete
Blocking dependencies: T07
Audit scope: A06 runtime follow-up

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz/src/deploy/planning.rs; ployz/src/deploy/planning/placement.rs; ployz-core/src/domain/deploy.rs

## Work

Track snapshot-local known host socket occupancy during candidate placement and planned operations; reuse host_ports_conflict, account for stop-first releases and alternative eligible Machines. Reject internally contradictory publications and unavoidable known cross-service conflicts before private plan construction. Preserve unknown external occupancy and runtime race reporting.

## Acceptance

Two distinct Services forced to one Machine cannot plan exclusive identical host sockets; compatible protocol/address distinctions and stop-first release work; different eligible Machines may satisfy the demand. No cluster-wide reservation/authority or new global state.

## Verification

Rung 2 existing deploy_plan placement tests with concrete one/two Machine snapshots and expected admitted/refused plans.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T08.md`; coordinator owns index/status updates and final four-axis review.
