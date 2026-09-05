# T10 — Validate row and document identity at persistence admission

Status: in_progress
Blocking dependencies: T01, T09
Audit scope: F05 persistence

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployzd/src/corrosion/store.rs; ployzd/src/corrosion/store/ingress.rs; ployzd/src/corrosion/store_tests.rs

## Work

Compare complete decoded Machine/Container/Volume identities against selected SQL row keys and local Machine filters. Centralize read admission at existing store helpers; preserve {} incomplete sentinel with original key. Return explicit malformed/unavailable evidence rather than silently rekeying or substituting identity. No generic typestate framework.

## Acceptance

Mismatched Machine ID, Container ID/owner and Machine-qualified volume name/owner never emerge as valid requested rows. Normal publishers and incomplete records still round-trip. No cross-record freshness/consistency check.

## Verification

Rung 1/2 existing ReplicatedStore read seam with malformed rows and incomplete sentinels. Use existing store adapter rather than new persistence abstraction.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T10.md`; coordinator owns index/status updates and final four-axis review.
