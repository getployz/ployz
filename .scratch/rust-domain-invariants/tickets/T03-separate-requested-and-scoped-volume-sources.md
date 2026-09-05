# T03 — Separate requested and scoped volume sources

Status: pending
Blocking dependencies: none
Audit scope: F02

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz-core/src/domain/volume.rs; ployz-core/src/domain/spec.rs; ployz-core/src/domain/service_graph.rs; ployz/src/compose/mounts.rs; ployz/src/deploy/planning/volumes.rs; ployzd/src/docker/volume.rs

## Work

Use distinct raw managed-volume declaration and privately constructed resolved/scoped source representations. Scope physical names and derive reserved ownership labels from the trusted representation. Raw input cannot supply reserved labels. Preserve exact observed physical identity during scale through an explicit checked import path. Update requested/resolved spec graph conversions, Docker creation, CLI/SDK constructors and tests atomically.

## Acceptance

Both foreign and same-Project forged labels fail raw input admission; ordinary data scopes once; scaling an observed source never prefixes twice or renames foreign observed ownership. External/bind/tmpfs semantics remain unchanged. Public mutation cannot impersonate scoped provenance.

## Verification

Rung 1 constructor/serde plus Rung 2 existing Compose/planner seam for forged labels and scale round-trip. Reuse named-volume and compose-normalize evidence paths.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T03.md`; coordinator owns index/status updates and final four-axis review.
