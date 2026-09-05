# T17 — Validate hosted DNS reservations

Status: pending
Blocking dependencies: T10
Audit scope: F12

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployzd/src/hosted_dns.rs; ployzd/src/corrosion/store.rs; ployz-core/src/domain/hostname.rs; related RPC payloads

## Work

Use private Reservation with validated DNS name and nonempty opaque role-specific token; HTTP response and persisted serde share fallible admission. Keep endpoint syntax validated with existing URL parser. Preserve explicit release/recovery and incomplete HTTP/DNS outcomes; avoid treating HTTP 2xx as semantic validity.

## Acceptance

Malformed successful response cannot persist empty/unusable identity or token and poison subsequent reservation. Malformed stored reservation is explicit invalid/unavailable evidence. Valid reserve/submit/release flow remains possible; names use proper domain grammar not overly narrow service-name grammar.

## Verification

Rung 1 reservation response/decode public admission; existing local hosted-DNS client seam if external transport behavior is needed.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T17.md`; coordinator owns index/status updates and final four-axis review.
