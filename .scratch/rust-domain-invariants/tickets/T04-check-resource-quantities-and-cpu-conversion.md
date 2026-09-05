# T04 — Check resource quantities and CPU conversion

Status: pending
Blocking dependencies: none
Audit scope: F06

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz-core/src/domain/spec.rs; ployz/src/compose/convert.rs; ployz/src/handlers/deploy_input.rs; ployzd/src/docker/create.rs; SDK payload conversion

## Work

Represent CPU nanounits and byte quantities with private checked nonnegative values compatible with Docker signed range. Share one CPU conversion between direct CLI and Compose; reject negative/nonfinite/out-of-range scaled results instead of saturating. Use checked serde and update consumers. Keep zero if supported. Leave ulimit/device negative sentinel domains unchanged.

## Acceptance

1e20 CPU input is rejected; normal fractional CPU converts exactly as declared; negative quantities fail SDK/RPC decoding; boundary overflow never silently saturates. Byte limits keep their valid upper bound and public API cannot bypass validation.

## Verification

Rung 1 checked quantities and conversion; Rung 2 existing Compose/input parsing seam. Exact rejected/accepted literals, not implementation-mirroring arithmetic assertions.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T04.md`; coordinator owns index/status updates and final four-axis review.
