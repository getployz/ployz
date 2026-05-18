---
title: Slice 038 p2panda 0.6 Canonical Transport Migration Plan
status: active
created: 2026-05-19
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution-audit.md
  - MVP/slice-037-p2panda-06-substrate-deletion.md
external:
  - https://docs.rs/p2panda-core/0.6.0/p2panda_core/
  - https://docs.rs/p2panda-store/0.6.0/p2panda_store/
  - https://docs.rs/p2panda-sync/0.6.0/p2panda_sync/
  - https://docs.rs/p2panda-net/0.6.0/p2panda_net/
  - https://docs.rs/p2panda-auth/0.6.0/p2panda_auth/
---

# Slice 038 p2panda 0.6 Canonical Transport Migration Plan

## Problem Frame

Slice 037 proved that crates.io `p2panda-net 0.6.0` is usable on the non-RC
iroh `0.98` family. The remaining blocker is local, not conceptual: active MVP
crates still carry p2panda `0.5.2`, `mvp-iroh` still pins iroh `0.96`, and
`mvp-p2panda-transport` still sends Ployz facts as an inner p2panda operation
wrapped inside a second transport operation body.

That wrapper stack is exactly the kind of substrate code this MVP is trying not
to maintain:

```text
PandaFactOperation
  -> PandaFactWireEnvelope / PFO1
  -> PandaNetQuarantineLog operation
  -> p2panda-net LogSync
  -> unwrap PFO1
  -> import PandaFactOperation
```

The target shape is one signed operation:

```text
Operation<PloyzFactExtensions>
  -> p2panda-store topic/log
  -> p2panda-net LogSync
  -> Ployz import policy
  -> FactSource / projections
```

This slice should make that migration real in the active MVP workspace. It is
acceptable to use non-RC iroh `0.98`; do not treat avoidance of iroh `1.0.0-rc`
as a reason to stay on the older p2panda line.

## Dependency Scout

Checked on 2026-05-19:

- `cargo search p2panda-net` reports `p2panda-net = "0.6.0"`.
- `cargo search iroh` reports `iroh = "1.0.0-rc.0"` as latest, but Slice 037's
  compile-backed spike resolved `p2panda-net 0.6.0` through non-RC
  `iroh 0.98.2` and `iroh-gossip 0.98.0`.
- The active workspace conflict is exact dependency resolution:
  `mvp-iroh -> iroh 0.96.1 -> ed25519-dalek =3.0.0-pre.1`, while
  `p2panda-net 0.6.0 -> iroh 0.98.2 -> ed25519-dalek =3.0.0-pre.6`.
- Therefore the first implementation move is not to invent a workaround around
  p2panda-net. It is to align or park the old direct-iroh proof so the
  maintained p2panda transport can become the active path.

## Scope

In scope:

- Upgrade the active MVP p2panda family from `0.5.2` to `0.6.0`.
- Align the active MVP iroh family to the non-RC `0.98` line where the existing
  direct-iroh proof still compiles.
- If `mvp-iroh` becomes a broad migration sink, park its historical E2E from
  `mvp-e2e all` and keep p2panda-net as the product transport path rather than
  spending the slice hardening two fact replication substrates.
- Replace live transport success paths so they move canonical
  `Operation<PloyzFactExtensions>` or `RawOperation` data, not `PFO1` wrapper
  bodies.
- Preserve branchable import outcomes: inserted, duplicate, conflict,
  deferred, rejected, and failed.
- Preserve projection rebuild behavior and process-serving proof behavior.
- Delete or quarantine `MVP/p2panda-06-spike` once the active workspace carries
  the same evidence.
- Update the deletion ledger in `MVP/slice-037-p2panda-06-substrate-deletion.md`
  and the decision ledger in `MVP/primitive-decisions.md`.

Out of scope:

- No new product feature.
- No p2panda-blobs adoption.
- No bus semantic changes.
- No quorum, witness acknowledgements, active-partition blocking, or strict
  lease mode.
- No migration outside `MVP/`.
- No direct dependency on iroh `1.0.0-rc.0` unless p2panda itself requires it
  later; this slice targets the known p2panda `0.6` / iroh `0.98` line.

## Implementation Units

### Unit 1: Align The Active Dependency Line

Files:

- `MVP/Cargo.toml`
- `MVP/Cargo.lock`
- `MVP/iroh/Cargo.toml`
- `MVP/p2panda-authz/Cargo.toml`
- `MVP/p2panda-facts/Cargo.toml`
- `MVP/p2panda-transport/Cargo.toml`
- `MVP/e2e/Cargo.toml`

Plan:

1. Upgrade active p2panda crates to `0.6.0`.
2. Upgrade `mvp-iroh` to the compatible non-RC iroh line when compile errors
   are local API drift.
3. If the direct-iroh proof pulls the slice away from p2panda deletion, remove
   it from product proof execution and document it as historical reference.
4. Keep raw p2panda and iroh dependency churn inside substrate crates. Product
   crates should continue to depend on Ployz-facing contracts.

Test scenarios:

- `cargo check -p mvp-p2panda-facts --all-targets`
- `cargo check -p mvp-p2panda-transport --all-targets`
- `cargo check -p mvp-p2panda-authz --all-targets`
- `cargo check --workspace`

### Unit 2: Promote Canonical Fact Operations

Files:

- `MVP/p2panda-facts/src/lib.rs`
- `MVP/p2panda-facts/src/tests.rs`
- `MVP/p2panda-06-spike/src/lib.rs`

Plan:

1. Move the Slice 037 compile-backed `PloyzFactExtensions` shape into the
   active fact crate, reusing the existing canonical identity types:
   `IslandId`, `FactKey`, `PrincipalId`, `FactContentHash`, and authority
   epoch data.
2. Ensure operation hash, payload hash, same-key conflict candidates,
   wrong-author rejection, cross-island rejection, and unauthorized writes still
   branch through structured outcomes.
3. Keep the `FactSource` boundary free of raw p2panda types.
4. Treat `PFO1` as legacy import/export only until Unit 4 proves it can be
   removed from success paths.

Test scenarios:

- Canonical operation round trip preserves island, key, author, content hash,
  authority epoch, and payload.
- Operation body tampering fails validation before Ployz import.
- Same key with different content remains visible as reducer conflict
  candidates.
- Duplicate canonical operations do not require wrapper replay cache behavior.

### Unit 3: Replace The Quarantine Transport Log

Files:

- `MVP/p2panda-transport/src/quarantine_log.rs`
- `MVP/p2panda-transport/src/node.rs`
- `MVP/p2panda-transport/src/fact_node.rs`
- `MVP/p2panda-transport/src/errors.rs`
- `MVP/p2panda-transport/src/tests.rs`

Plan:

1. Replace `PandaNetQuarantineLog` as the live success-path store with
   p2panda-store topic/log support for canonical fact operations.
2. Preserve bounded body-size rejection, pending out-of-order import retry, and
   explicit failure when the pending queue is exhausted.
3. Preserve replay handling through canonical operation identity. If p2panda
   stream refresh still replays already-seen operations, suppress by canonical
   operation hash, not wrapper hash.
4. Keep rejection reporting in Ployz. p2panda owns transport/storage mechanics;
   Ployz owns authorization and operator/audit statuses.

Test scenarios:

- Live p2panda-net delivery imports a canonical fact into the receiver's
  `SharedPandaFactStore`.
- Duplicate live delivery reports duplicate, not inserted.
- Wrong author, wrong island, unauthorized replica, malformed body, oversized
  body, out-of-order predecessor, and pending queue full stay branchable.
- No test success path encodes `PFO1`.

### Unit 4: Update Product Proofs And Delete Dead Scaffolding

Files:

- `MVP/e2e/src/p2panda_net_fact_node_contract.rs`
- `MVP/e2e/src/p2panda_net_process_serving_contract.rs`
- `MVP/e2e/src/p2panda_acme_http01_contract.rs`
- `MVP/e2e/src/machine_remove_contract.rs`
- `MVP/e2e/src/main.rs`
- `MVP/p2panda-06-spike`
- `MVP/slice-037-p2panda-06-substrate-deletion.md`
- `MVP/primitive-decisions.md`
- `MVP/overall-plan.md`

Plan:

1. Move the Slice 037 spike assertions into active crate tests or E2Es.
2. Delete the excluded spike crate once active workspace tests cover the same
   API fit.
3. Remove or quarantine `PFO1` and `PandaFactWireEnvelope` after active E2Es
   prove canonical transport.
4. Retire any historical iroh-docs proof from `mvp-e2e all` only after the
   p2panda canonical path covers conflict, unauthorized, rebuild, and
   process-serving semantics.
5. Update the decision ledger with the exact code deleted and the exact code
   deliberately retained.

Test scenarios:

- `cargo run -p mvp-e2e -- p2panda-net-fact-node-contract`
- `cargo run -p mvp-e2e -- p2panda-net-process-serving-contract`
- `cargo run -p mvp-e2e -- p2panda-acme-http01-contract`
- `cargo run -p mvp-e2e -- machine-remove-contract`
- `cargo run -p mvp-e2e -- all`

## Success Criteria

- The active MVP workspace uses p2panda `0.6.0` and non-RC iroh `0.98` where
  iroh remains active.
- p2panda-net live transport moves canonical fact operations, not wrapper
  envelopes.
- `PandaNetQuarantineLog` is deleted or reduced to a clearly named legacy test
  fixture outside product success paths.
- `PFO1` / `PandaFactWireEnvelope` are deleted from product success paths.
- Branchable import outcomes and projection rebuild behavior remain unchanged.
- The excluded `MVP/p2panda-06-spike` crate is deleted after its evidence is
  absorbed into active tests.
- The slice report includes LOC/dependency deletion numbers and explicit
  retained-code rationale.

## Review Focus

- Do not accept a migration that only renames the wrapper layer. The live
  success path must have one signed p2panda operation.
- Do not let raw p2panda or iroh types leak into deploy, ACME, machine,
  environment, routing, or serving product crates.
- Do not weaken structured rejection outcomes to make the migration fit.
- Prefer deleting local substrate even if p2panda APIs are pre-`1.0`, as long
  as the Ployz-facing boundary stays small and tested.
