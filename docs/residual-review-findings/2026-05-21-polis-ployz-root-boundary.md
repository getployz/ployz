---
date: 2026-05-21
source: ce-code-review
plan: docs/plans/2026-05-21-003-refactor-polis-ployz-root-api-boundary-plan.md
status: open
---

# Polis/Ployz Root Boundary Residual Review Findings

The review fixes applied in this slice addressed correctness and test issues:
runtime receipt validation, volume ownership invariant checks, durable cleanup
failure evidence before terminal success, certificate freshness preserving
unknown, stricter boundary regexes, clippy cleanliness, and clearer
`ployz-e2e` binary behavior.

Remaining findings are intentionally not auto-fixed in this plan:

- The unused speculative `MutationReceiptStore` surface was removed with
  `polis::calls`, and the unused public projection/record extension traits
  (`RecordAuthorizer`, `Reducer`, and `ProjectionStore`) and their unused
  `RawRecord` / `RecordSource` support types were deleted in the Polis API
  simplification slice. Reintroduce bounded call receipts or projection store
  seams only from a real product caller.
- The active `crates/ployz-e2e` crate is in-process product acceptance, not a
  real daemon/substrate E2E harness. A future runner should be added only when
  real process, runtime, gateway, ACME, network, or volume substrate boundaries
  exist in the root rewrite.
- Ployz duplicates Polis-neutral identity and operation types by design to keep
  product modules from depending on Polis directly. Real adapter work should add
  focused conversion helpers so this does not become scattered manual mapping.
