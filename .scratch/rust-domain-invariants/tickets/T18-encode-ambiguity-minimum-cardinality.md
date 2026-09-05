# T18 — Encode ambiguity minimum cardinality

Status: complete
Blocking dependencies: none
Audit scope: F14

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz-core/src/domain/mod.rs; ployz-core/src/domain/machine.rs; ployz/src/cluster.rs; ployz/src/handlers/volume.rs

## Work

Represent Ambiguous with required first and second candidates plus remainder, or equally small private representation enforcing at least two in Rust and serde. Update from_matches and all consumers with a simple iteration helper. Avoid generic nonempty collection framework.

## Acceptance

Zero and one values classify None/One; Ambiguous cannot be constructed/deserialized with fewer than two. Multiple candidates and ordering remain preserved. No false claim of global name uniqueness.

## Verification

Rung 1 public classification/serde, existing selector/volume behavior tests adjusted.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T18.md`; coordinator owns index/status updates and final four-axis review.
