# T07 — Make executable plans private and derive progress

Status: in_progress
Blocking dependencies: T06
Audit scope: F04

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz-core/src/domain/deploy.rs; ployz/src/deploy/pipeline.rs; ployz/src/deploy/progress.rs; ployz/src/deploy/exec/mod.rs; ployz/src/sdk.rs; ployz-sdk/src/lib.rs

## Work

Separate privately admitted ordered executable operations from public preview/progress payloads. Construct execution state from operations, deriving target/index/Pending status; confirmation cannot execute arbitrary progress rows. Update CLI and SDK directly while preserving the prepared handle pattern, intentional replay, ordered partial outcomes and preflight exceptions.

## Acceptance

No supported constructor/serde path can execute a row reporting Machine A while operating on B or accept completed status as an executable plan. Preview rendering and progress sequence retain accurate targets and outcomes. Actual runtime feasibility remains fallible.

## Verification

Rung 1 plan/row public interface or Rung 2 existing pipeline/exec seam. Exercise confirmation/progress observation, not private field assertions.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T07.md`; coordinator owns index/status updates and final four-axis review.
