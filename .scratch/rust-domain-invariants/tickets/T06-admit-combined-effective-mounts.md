# T06 — Admit combined effective mounts

Status: pending
Blocking dependencies: T00, T03, T05
Audit scope: F08

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz-core/src/domain/service_graph.rs; ployz-core/src/domain/spec.rs; ployzd/src/docker/lifecycle.rs; ployzd/src/docker/create.rs

## Work

Implement the T00 decision in a privately constructed combined mount aggregate within requested/resolved specs. Validate volume/config effective targets together including defaults; carry established relationship through scoped resolution. Expose consumers through coherent accessors. Remove independent public mutation that could reintroduce conflicting graphs. Use the smallest aggregate that supports existing reference lookup.

## Acceptance

T00 conflict cases fail common serde/construction before Docker materialization; repeated sources at different destinations work; config defaults use exactly the same rule as admission; no compatibility wrapper or duplicate validation system.

## Verification

Rung 1 spec admission; Rung 2 existing Compose/materialization seam only when default resolution cannot be tested honestly lower.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T06.md`; coordinator owns index/status updates and final four-axis review.
