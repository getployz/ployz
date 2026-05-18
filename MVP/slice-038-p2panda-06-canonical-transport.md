---
title: Slice 038 p2panda 0.6 Canonical Transport
status: completed
created: 2026-05-19
origin:
  - MVP/slice-038-p2panda-06-canonical-transport-plan.md
  - MVP/slice-037-p2panda-06-substrate-deletion.md
  - MVP/design-notes/p2panda-substitution-audit.md
---

# Slice 038 p2panda 0.6 Canonical Transport

## Result

The active MVP workspace now uses the p2panda `0.6.0` line and the non-RC iroh
`0.98` line. Avoiding iroh `1.0.0-rc` does not block using `p2panda-net`.

The live `PandaNetFactNode` success path now publishes and imports canonical
`p2panda_core::Operation<PandaFactExtensions>` values against
`SharedPandaFactStore`. The product proof no longer sends a `PFO1` body through
a second transport operation before importing the fact.

## What Changed

- `mvp-p2panda-facts` exposes the canonical fact extension/log shape needed by
  `p2panda-net` and `p2panda-store`.
- `SharedPandaFactStore` implements the p2panda store traits needed by live
  `LogSync`.
- `mvp-p2panda-transport::PandaNetFactNode` uses `p2panda-net 0.6.0` with
  canonical fact operations in its live path.
- Missing-body, oversized, wrong-author, unauthorized, duplicate, conflict, and
  out-of-order cases still surface as branchable Ployz import outcomes.
- Stream refresh now drops the old subscription before opening a new one, so a
  refreshed receiver does not close the fresh stream by dropping stale state.
- Rejected operations no longer poison replay suppression; a later valid
  operation with the same payload identity can still be evaluated by the store.
- Process-serving E2E proves delayed remote update, one rejected operation,
  last-good serving state, projection rebuild, and restart from persistent
  p2panda state while the coordinator socket is absent.

## Retained Code

`PandaFactWireEnvelope`/`PFO1`, `PandaNetNode`, `PandaNetQuarantineLog`, and the
wire-body harness still exist. They are no longer the live fact-node success
path, but older E2Es and direct import probes still use them.

That retained code is now deletion debt, not an architecture direction. The
next p2panda substitution slice should decide whether to:

- delete the opaque-body `PandaNetNode`/quarantine path,
- quarantine it behind a legacy fixture module,
- or replace the remaining E2Es with canonical `PandaNetFactNode` coverage and
  remove the public exports.

## Verification

Passed before shipping the slice:

```text
cd MVP && cargo check --workspace
cd MVP && cargo test -p mvp-p2panda-facts
cd MVP && cargo test -p mvp-p2panda-transport
cd MVP && cargo run -p mvp-e2e -- p2panda-fact-source-contract
cd MVP && cargo run -p mvp-e2e -- p2panda-net-fact-node-contract
cd MVP && cargo run -p mvp-e2e -- p2panda-net-process-serving-contract
```

## Next Slice Trigger

Do not add another product feature until the remaining p2panda substitution
surface has been audited against the new canonical transport reality. The
highest-value question is no longer "can we use p2panda-net without RC iroh?"
That is proven. The question is which MVP-local plumbing can now be deleted,
which p2panda crate should own it instead, and which Ployz semantics must stay
above p2panda.
