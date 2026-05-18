---
title: Slice 040 Delete Opaque p2panda-net Transport Plan
status: active
created: 2026-05-19
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-039-p2panda-substitution-deletion-audit.md
---

# Slice 040 Delete Opaque p2panda-net Transport Plan

## Problem Frame

Slice 038 proved the active p2panda-net path can move canonical
`Operation<PandaFactExtensions>` values through `PandaNetFactNode` on non-RC
iroh `0.98`. Slice 039 showed the old opaque-body transport path still remains
as active source and E2E scaffolding:

```text
PandaFactOperation
  -> PandaFactWireEnvelope / PFO1
  -> PandaNetNode
  -> PandaNetQuarantineLog
  -> transport_wire_bodies
  -> import_fact_body
```

That is duplicate substrate. It reimplements p2panda log/topic storage,
maintains a second network node API, and keeps byte-envelope import paths alive
after canonical p2panda operation transport is already working.

This slice deletes the opaque path and keeps the canonical fact-node path.

## Dependency Scout

Checked from the active workspace on 2026-05-19:

- `p2panda-net 0.6.0` already provides iroh endpoint, address book, discovery,
  gossip, sync, and optional supervision.
- `PandaNetFactNode` already uses `LogSync<SharedPandaFactStore,
  PandaFactLogId, PandaFactExtensions>` and canonical
  `Operation<PandaFactExtensions>`.
- `SharedPandaFactStore` already implements the p2panda store traits needed by
  `PandaNetFactNode`.
- No new crate is needed. The simplification is deletion plus moving any
  remaining direct probes to canonical-operation helpers.

Do not add another abstraction unless deletion exposes a real shared helper
needed by `PandaNetFactNode`.

## Scope

In scope:

- Delete `PandaFactWireEnvelope`, `PandaFactWireEnvelopeError`, and `PFO1` from
  active fact code.
- Delete `import_fact_body` and `import_fact_body_into_shared_store` from the
  normal transport API.
- Delete `PandaNetNode`, `PandaNetStream`, `PandaNetQuarantineLog`,
  `PandaNetStore`, and `MVP/p2panda-transport/src/quarantine_log.rs`.
- Keep `PandaNetNodeConfig`, `PandaNetNodeSeed`, `PandaNetNodeInfo`,
  `PandaNetNodeTicket`, `PandaNetTopic`, `PandaNetNetworkId`, startup helpers,
  and replay cache pieces if `PandaNetFactNode` still uses them.
- Replace E2E callers that use `transport_wire_bodies` with canonical
  `PandaNetFactNode` flows or direct canonical operation import probes.
- Preserve branchable import outcomes and process-serving behavior.
- Update maintainer docs and proof docs so no current plan claims `PFO1` is a
  product path.

Out of scope:

- No p2panda-auth membership work.
- No p2panda discovery/address-book adoption beyond existing fact-node use.
- No root workspace or existing `crates/` changes.
- No product-feature behavior changes.
- No removal of bus-backed deploy canaries until equivalent p2panda-backed
  deploy canaries exist.
- No removal of `mvp-iroh` as a crate; this slice only removes the opaque
  p2panda-net transport path.

## Implementation Units

### Unit 1: Add Canonical Direct-Import Test Helpers

Files:

- `MVP/p2panda-transport/src/fact_driver.rs`
- `MVP/p2panda-transport/src/harness.rs`
- `MVP/p2panda-transport/src/lib.rs`
- `MVP/p2panda-transport/src/tests.rs`

Plan:

1. Keep `import_p2panda_operation_into_shared_store` as the canonical import
   core.
2. If E2Es need direct rejection probes without live network, expose a
   harness-gated canonical helper that accepts
   `Operation<PandaFactExtensions>`.
3. Do not export a normal public byte-body import API.
4. Preserve `PandaNetFactImportOutcome` and its branchable rejection/failure
   variants.

Test scenarios:

- Direct canonical operation import rejects unauthorized replica.
- Direct canonical operation import rejects author-key mismatch.
- Header-only canonical operation is `Rejected(MalformedOperation)`, not a
  local failure.

### Unit 2: Replace Fact-Node Direct `PFO1` Probes

Files:

- `MVP/e2e/src/p2panda_net_fact_node_contract.rs`
- `MVP/p2panda-transport/src/tests.rs`

Plan:

1. Replace the unauthorized-replica probe currently built with
   `PandaFactWireEnvelope::encode`.
2. Replace the author-key mismatch probe currently built with
   `PandaFactWireEnvelope::encode`.
3. Keep the live sync body of `p2panda-net-fact-node-contract` unchanged unless
   cleanup reveals a simpler canonical helper.

Test scenarios:

- `p2panda-net-fact-node-contract` still reports:
  - inserted facts,
  - conflict candidate,
  - untrusted author rejection,
  - cross-island rejection,
  - unauthorized replica rejection,
  - author mismatch rejection,
  - no deferred or failed outcomes.

### Unit 3: Replace Or Remove Opaque-Body E2Es

Files:

- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/p2panda_net_sync_contract.rs`
- `MVP/e2e/src/p2panda_net_owned_node_contract.rs`
- `MVP/e2e/src/p2panda_acme_http01_contract.rs`

Plan:

1. Decide per scenario whether it is still proving a distinct product
   invariant after `p2panda-net-fact-node-contract` and
   `p2panda-net-process-serving-contract`.
2. If a scenario only proves opaque-body transport, remove it from `SCENARIOS`
   and delete the file.
3. If a scenario still proves product behavior, port it to canonical
   `PandaNetFactNode`.
4. For `p2panda-net-acme-http01-contract`, prefer canonical fact-node replay.
   If the non-process ACME net contract is now redundant with
   `p2panda-net-process-serving-contract` plus `p2panda-acme-http01-contract`,
   delete the redundant net variant and document the coverage.

Test scenarios:

- The remaining E2E list still covers:
  - canonical net fact transport,
  - process-role canonical net serving,
  - p2panda ACME HTTP-01 behavior,
  - p2panda sync/fact-source behavior.
- `mvp-e2e -- all` has no success path that calls `transport_wire_bodies`.

### Unit 4: Delete Opaque Transport Code

Files:

- `MVP/p2panda-facts/src/lib.rs`
- `MVP/p2panda-transport/src/lib.rs`
- `MVP/p2panda-transport/src/node.rs`
- `MVP/p2panda-transport/src/quarantine_log.rs`
- `MVP/p2panda-transport/src/harness.rs`
- `MVP/p2panda-transport/src/errors.rs`
- `MVP/p2panda-transport/src/tests.rs`

Plan:

1. Delete the `PFO1` codec and its unit test.
2. Delete `quarantine_log.rs` and remove the module.
3. Delete opaque `PandaNetNode` and `PandaNetStream`.
4. Keep shared typed wrappers/config helpers used by `PandaNetFactNode`.
5. Remove stale error variants and imports that only served quarantine/body
   transport.
6. Run `rg` to prove no product source references remain.

Test scenarios:

- `cargo test -p mvp-p2panda-facts`
- `cargo test -p mvp-p2panda-transport`
- `rg "PandaFactWireEnvelope|PFO1|PandaNetQuarantineLog|transport_wire_bodies|import_fact_body" MVP/p2panda-facts MVP/p2panda-transport MVP/e2e/src`
  returns no active source references. Historical markdown references are fine.

### Unit 5: Update Proof And Decision Docs

Files:

- `MVP/slice-040-delete-opaque-p2panda-net-transport.md`
- `MVP/overall-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/e2e-proof-plan.md`
- `MVP/design-notes/p2panda-substitution-audit.md`
- `MVP/design-notes/semantic-leverage-loc.md`

Plan:

1. Record exactly what was deleted.
2. Record any retained fixture and its deletion trigger. The default target is
   no retained fixture.
3. Record semantic-leverage impact: deleted LOC, remaining product proof, and
   which p2panda crate now owns the removed mechanics.
4. Mark Slice 040 as complete only after tests pass.

## Success Criteria

- No active Rust source in `MVP/p2panda-facts`, `MVP/p2panda-transport`, or
  `MVP/e2e/src` references `PandaFactWireEnvelope`, `PFO1`,
  `PandaNetQuarantineLog`, `transport_wire_bodies`, or `import_fact_body`.
- `PandaNetFactNode` remains the only p2panda-net fact transport used by
  product-shaped E2Es.
- Branchable import outcomes are preserved.
- Process-serving with canonical p2panda-net still proves delayed remote
  update, rejected import, projection rebuild, restart, and missing coordinator
  socket behavior.
- No Ployz product crate learns raw p2panda transport details beyond existing
  substrate boundaries.

## Verification

Targeted gates:

```text
cd MVP && cargo check --workspace
cd MVP && cargo test -p mvp-p2panda-facts
cd MVP && cargo test -p mvp-p2panda-transport
cd MVP && cargo run -p mvp-e2e -- p2panda-net-fact-node-contract
cd MVP && cargo run -p mvp-e2e -- p2panda-net-process-serving-contract
cd MVP && cargo run -p mvp-e2e -- p2panda-acme-http01-contract
cd MVP && cargo run -p mvp-e2e -- all
```

Deletion gate:

```text
rg "PandaFactWireEnvelope|PFO1|PandaNetQuarantineLog|transport_wire_bodies|import_fact_body" MVP/p2panda-facts MVP/p2panda-transport MVP/e2e/src
```

The deletion gate should return no active source references. If it returns a
legacy fixture, the implementation report must name why it stayed and when it
will be removed.

## Review Focus

- Watch for accidental weakening of import outcomes while removing the byte
  envelope.
- Watch for keeping old code under a softer name instead of deleting it.
- Watch for deleting a product proof instead of replacing it with canonical
  coverage.
- Watch for raw p2panda types leaking into product crates.
- Watch for `mvp-e2e -- all` getting faster by silently dropping product
  behavior coverage.
