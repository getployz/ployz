# T12 — Derive Runtime Watch Services from Containers

Status: complete
Blocking dependencies: T09
Audit scope: F13

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz-core/src/domain/runtime_watch.rs; ployzd/src/runtime_watch.rs; ployz/src/sdk.rs; ployz-sdk-payloads

## Work

Remove redundant stored services from RuntimeWatchFrame and derive on access from Containers. Update daemon assembly, encode/decode, SDK consumers and generated declarations with one coherent representation. Avoid multiple DTO layers or a private cache over publicly mutable Containers.

## Acceptance

Consumers see Services derived from frame Containers; no public mutation can leave a stored Service view stale. Wire decoding preserves unknown/partial observation behavior. Existing Watch reconnect semantics unaffected.

## Verification

Rung 1 frame public decode/derive seam, existing sdk-runtime-watch tests updated only as necessary.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T12.md`; coordinator owns index/status updates and final four-axis review.
