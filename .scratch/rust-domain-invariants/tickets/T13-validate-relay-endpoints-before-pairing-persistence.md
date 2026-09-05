# T13 — Validate Relay endpoints before pairing persistence

Status: complete
Blocking dependencies: none
Audit scope: F09

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz-core/src/domain/machine.rs; ployz-relay/src/client.rs; ployzd/src/relay.rs; ployz/src/cloud_enroll.rs; ployz/src/context.rs

## Work

Introduce/reuse a private HTTP(S) Relay endpoint value admitted with installed URL parser; use it in CloudPairing and Relay client input. Apply same admission to serde/enrollment/authorized RPC. Update payloads and consumers. Keep endpoint availability, auth and Register state separate.

## Acceptance

Garbage, unsupported schemes and structurally unusable HTTP(S) endpoints fail before pairing persistence; valid endpoints work. No reachability requirement, no cross-role bearer reuse, no migration fallback.

## Verification

Rung 1 endpoint/CloudPairing constructor and serde; existing Relay client tests as needed.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T13.md`; coordinator owns index/status updates and final four-axis review.
