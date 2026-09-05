# T01 — Derive Machine identity and validate local lifecycle payloads

Status: complete
Blocking dependencies: none
Audit scope: F05 Machine

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz-core/src/domain/machine.rs; ployzd/src/machine/mod.rs; ployzd/src/network.rs; ployzd/src/network/linux.rs; all Machine constructors/consumers

## Work

Derive management address from public key rather than retaining independently mutable derived data. Privately own coherent local lifecycle/key state and checked nonempty join/bootstrap payloads. Preserve assigned durable Machine ID semantics. Update producers, serde, consumers and tests; derive local public-key facts from private material or establish them through one private constructor. Preserve phase-specific inspection and cleanup access, and do not change Joining operation policy.

## Acceptance

Malformed local state cannot be admitted with inconsistent key identity or empty required join payload. Normal initialize/join/reset inspection still works. No arbitrary public struct construction can bypass the chosen local invariant; remote observations retain unknown/stale semantics.

## Verification

Rung 1 core/local Machine public constructors and decoding; existing local lifecycle integration seam if needed. Name exact checks in handoff.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T01.md`; coordinator owns index/status updates and final four-axis review.
