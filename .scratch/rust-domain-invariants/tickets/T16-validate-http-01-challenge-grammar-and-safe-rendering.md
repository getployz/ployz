# T16 — Validate HTTP-01 challenge grammar and safe rendering

Status: complete
Blocking dependencies: T15
Audit scope: F11 challenge

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployzd/src/corrosion/certificate.rs; ployzd/src/certificates.rs; ployzd/src/ingress/caddy.rs; ployzd/src/ingress/envoy.rs; ployzd/src/ingress/zentinel.rs

## Work

Private challenge constructor establishes token/key-authorization syntax and correspondence using protocol-supported grammar; both local issuance and persisted decode use it. Encode challenge values safely at configuration sinks, using existing backend encoding patterns. Preserve simultaneous material+challenge renewal state and isolated per-backend safety checks.

## Acceptance

Malformed/empty token or authorization, mismatched token prefix and configuration-control text are rejected before projection; valid issuance output renders on every backend. No invented single-certificate-status enum; no bespoke crypto parser.

## Verification

Rung 1 challenge constructor/serde and backend public projection checks with malicious literals and valid token. External protocol facts consult primary sources when necessary.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T16.md`; coordinator owns index/status updates and final four-axis review.
