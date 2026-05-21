---
title: Slice 024 ACME Command Surface
status: completed
created: 2026-05-18
---

# Slice 024 ACME Command Surface

Slice 024 extracted the ACME HTTP-01 command behavior from the p2panda E2E
scenario into `mvp-acme-command`.

The reusable surface now owns:

- challenge lease claim,
- HTTP-01 present,
- HTTP-01 clear,
- lease-state replay from `FactSource`,
- preflight-before-mutation checks,
- visible nodes at decision time,
- structured command errors.

The p2panda ACME E2Es now keep transport, projection, serving, and fixture
work. They no longer own the ACME command state machine.

## Semantic Leverage

Old-code baseline:

- `crates/ployzd/src/daemon/cert_coordination.rs`: 520 LOC
- `crates/ployz-cert-backends/src/*.rs`: 535 LOC
- total old cert coordination/backend baseline: 1,055 LOC

Slice result:

- `MVP/acme-command/src/lib.rs`: 526 LOC
- `MVP/acme-command/src/p2panda.rs`: 133 LOC
- `MVP/acme-command/src/tests.rs`: 468 LOC
- `MVP/e2e/src/p2panda_acme_http01_contract.rs`: 1,218 LOC, down from
  roughly 1,653 LOC at slice start.

That is not a final production ACME implementation. It deliberately excludes
real ACME account/order/challenge protocol work. The proof is that the command
semantics are now reusable business logic instead of E2E-local substrate glue.

`instant-acme` remains the likely future protocol-client candidate when real
issuance becomes the proof target. It was not added here because this slice was
about Ployz command/fact semantics, not RFC 8555 integration.

## Review Fixes

The review pass found useful issues and they were fixed before closeout:

- malformed, unreadable, and missing-payload lease candidates now have focused
  command tests;
- stale clear is tested separately from stale present;
- clear has symmetric coverage for release-preflight failure before any write;
- ACME command no longer depends on lease harness features;
- lease key/payload validation reuses the projection reducer helper instead of
  maintaining a second parser;
- E2E-only stale/candidate-count helpers stay in the E2E fixture layer.

## Verification

Passed:

```bash
cargo test -p mvp-acme-command
cargo check -p mvp-e2e
cargo run -p mvp-e2e -- p2panda-acme-http01-contract
cargo run -p mvp-e2e -- p2panda-net-acme-http01-contract
cargo run -p mvp-e2e -- docs-backed-acme-http01-contract
cargo clippy -p mvp-acme-command -p mvp-e2e --all-targets -- -D warnings
MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```
