# T11 — Retain resource identity in volume removal outcomes

Status: pending
Blocking dependencies: none
Audit scope: F03

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz/src/cluster.rs; ployz-core/src/domain/volume.rs; ployz-core/src/rpc.rs; ployz/src/handlers/volume.rs; ployz/src/sdk.rs; ployz-sdk/src/lib.rs; ployz-sdk-payloads

## Work

Replace Machine-only removal PartialResult with operation-specific outcomes carrying required DockerVolumeId and success/failure/omission enum. Keep every requested resource identifiable including generic timeout and omissions. Update all callers, CLI rendering, SDK payload/type generation directly. Retain valid mixed outcomes for several volumes on one Machine.

## Acceptance

Two failed volumes on one Machine retain distinct IDs with no dependence on error text or input ordering. Mixed success/failure and omission identify exact targets. Timed-out completion remains unknown; no Machine-unique map.

## Verification

Rung 2 existing volume/client semantic seam with two volume identities and generic errors; Rung 3 CLI shape only if needed to verify preserved visible names.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T11.md`; coordinator owns index/status updates and final four-axis review.
