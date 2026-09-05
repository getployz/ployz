# T05 — Require nonempty pre-deploy hook command

Status: pending
Blocking dependencies: T04
Audit scope: F07

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz-core/src/domain/spec.rs; ployz/src/compose/extensions.rs; ployzd/src/docker/create.rs; ployz/src/deploy/exec/mod.rs

## Work

Replace public hook Vec command with a private nonempty command value and checked serde, following the existing HealthcheckCommand pattern without its probe-specific sentinel rule. Update Compose, SDK/RPC, Docker conversion and fixtures. Preserve valid entrypoint semantics.

## Acceptance

Empty hook command is rejected by every input route before execution; valid commands remain representable, including words that are not forbidden outside healthchecks. No generic nonempty collection framework.

## Verification

Rung 1 hook constructor/serde with malformed SDK-shaped payload; existing Compose test updated only as needed.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T05.md`; coordinator owns index/status updates and final four-axis review.
