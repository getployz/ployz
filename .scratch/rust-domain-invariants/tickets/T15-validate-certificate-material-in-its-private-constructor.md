# T15 — Validate certificate material in its private constructor

Status: complete
Blocking dependencies: none
Audit scope: F11 material

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployzd/src/corrosion/certificate.rs; ployzd/src/certificates.rs; ployzd/src/ingress/zentinel.rs; ployzd/src/ingress.rs

## Work

Strengthen private CertificateMaterial construction/serde to require parseable certificate/key and matching pair using already installed crypto/parser facilities. Support the actual issuance policy key algorithms; identify supported algorithms from source, do not silently narrow. Use checked admission for stored decode and local publication, preserve invalid/unavailable evidence. Reuse existing pair-validation logic where appropriate.

## Acceptance

Nonempty garbage and mismatched cert/key cannot become valid material; supported generated key types continue to work. Material presence still does not assert trust/hostname/date/proxy adoption. Unparseable persisted material cannot suppress repair by masquerading as valid.

## Verification

Rung 1 material constructor/decode using existing rcgen/parser facilities and fixed expected success/refusal; existing acme-certs evidence seam if required.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T15.md`; coordinator owns index/status updates and final four-axis review.
