---
title: Slice 037 p2panda 0.6 Substrate Deletion Plan
status: active
created: 2026-05-19
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution-audit.md
  - MVP/slice-034-p2panda-auth-and-substrate-simplification.md
  - MVP/slice-035-p2panda-authz-fact-authority.md
  - MVP/slice-036-phased-command-primitive.md
external:
  - https://docs.rs/p2panda-core/0.6.0/p2panda_core/
  - https://docs.rs/p2panda-store/0.6.0/p2panda_store/
  - https://docs.rs/p2panda-sync/0.6.0/p2panda_sync/
  - https://docs.rs/p2panda-net/0.6.0/p2panda_net/
  - https://docs.rs/p2panda-auth/0.6.0/p2panda_auth/
  - https://docs.rs/p2panda-blobs/0.5.2/p2panda_blobs/
  - https://github.com/p2panda/p2panda/releases/tag/v0.6.0
---

# Slice 037 p2panda 0.6 Substrate Deletion Plan

## Problem Frame

The MVP has already biased hard toward p2panda:

- p2panda signed operations are the durable fact substrate;
- p2panda-store SQLite persists local fact logs;
- p2panda-sync moves missing operations between stores;
- p2panda-net carries fact operations over the maintained iroh/gossip/log-sync
  stack;
- p2panda-auth now feeds `IslandAuthoritySnapshot` for the product fact path.

The remaining risk is not that p2panda is too immature. The larger risk is that
Ployz keeps accumulating young custom substrate next to a maintained project
that is solving the same plumbing better. p2panda `0.6.0` is now published on
crates.io and `p2panda-net 0.6.0` depends on the non-RC iroh `0.98` family.
That removes the earlier "we need to wait for a non-RC transport line" reason
for staying on `0.5.2`.

This slice is a deep, compile-backed deletion investigation. It should answer
which custom pieces can be replaced early by upgrading the MVP p2panda line to
`0.6.0`, and which product semantics still need Ployz-owned code.

The output is not a product feature. The output is a migration map with compile
evidence strong enough that the following slice can delete code instead of
speculating.

## Crate Scout

Checked on 2026-05-19:

- `p2panda-core 0.6.0` still provides signed append-only operations,
  BLAKE3-backed hashes, typed custom header extensions, and operation
  validation. `RawOperation` remains available as the encoded operation shape.
- `p2panda-store 0.6.0` provides SQLite-backed store traits, including
  operation/log/topic/group storage needed by the current fact, sync, network,
  and authz paths.
- `p2panda-sync 0.6.0` continues to own generic log synchronization. It should
  remain the store-to-store catch-up primitive instead of introducing another
  anti-entropy loop.
- `p2panda-net 0.6.0` is the strongest new opportunity. Its docs describe
  event delivery, gossip, discovery, iroh endpoint connectivity, supervisor
  modules, and eventual-consistent `LogSync`. It now depends on `iroh 0.98.2`,
  `iroh-gossip 0.98.0`, `p2panda-core 0.6.0`, `p2panda-store 0.6.0`, and
  `p2panda-sync 0.6.0`.
- `p2panda-auth 0.6.0` carries the group/membership direction from Slice 034 and
  Slice 035 forward. The likely adoption remains: p2panda owns group CRDT
  semantics; Ployz owns principal/key/epoch binding and fact-key grants.
- `p2panda-blobs 0.5.2` is not ready as the main payload substrate for this
  MVP. The crate still appears to target the older p2panda-net API line and its
  source notes that it needs refactoring after the p2panda-net refactor. Keep
  blob adoption out of this slice unless a compile spike proves otherwise.
- Upstream p2panda is pre-`1.0`; API churn is expected. That is acceptable for
  this MVP if the migration deletes more local substrate than it adds. The
  comparison is maintained p2panda code versus custom MVP code, not versus a
  stable perfect abstraction.

## Current Custom Substrate To Challenge

| Candidate | Current location | What p2panda 0.6 might replace | Desired result |
| --- | --- | --- | --- |
| `PandaFactWireEnvelope` / `PFO1` | `MVP/p2panda-transport`, `MVP/p2panda-facts` callers | Sync canonical `Operation<PloyzFactExtensions>` or `RawOperation` directly through `p2panda-net::sync::LogSync` instead of wrapping fact operations in another signed p2panda operation body. | Delete the envelope or shrink it to a versioned debug/export format only. |
| `PandaNetQuarantineLog` | `MVP/p2panda-transport/src/quarantine_log.rs` | Use p2panda-store topic/log traits and p2panda-net `LogSync` with the canonical fact store instead of a separate wrapper-operation topic map. | Delete wrapper-log signing and keep only Ployz import rejection/status reporting. |
| Manual trusted-author fallback | `MVP/p2panda-facts/src/lib.rs`, E2E fixtures | Current product path already uses `IslandAuthoritySnapshot`; p2panda 0.6 may make authz replay/store integration simpler enough to remove or feature-gate the fallback. | Move manual trust APIs behind harness-only use or delete product callers. |
| Manual trusted-replica fallback | `MVP/p2panda-facts/src/lib.rs`, E2E fixtures | p2panda-auth group state plus Ployz `ReplicaImporter` condition. | Product paths import only through authz-derived replica authority. |
| Historical iroh-docs proof | `MVP/iroh/src/facts.rs`, `iroh-docs-contract` E2E | p2panda-net/store/sync now cover durable append-log replication and product canaries. | Retire from `mvp-e2e all` or park as historical reference after equivalent p2panda 0.6 proof. |
| Process JSON fact source | `MVP/e2e/src/process_fact_source.rs` | Persistent p2panda stores already prove role restart; p2panda-net process serving proves remote update flow. | Keep only if it covers a process-fate case not covered by p2panda stores; otherwise retire. |
| Bus fact store as product-shaped fact substrate | `MVP/bus/src/facts.rs`, `MVP/projection/src/bus_source.rs` | p2panda-backed adapters now cover product fact paths. | Keep as a local fixture, not a product proof path. |

## Scope

In scope:

- Add a temporary, isolated `MVP/p2panda-06-spike` crate or equivalent
  workspace-local compile spike using dependency aliases such as
  `p2panda-core-06 = { package = "p2panda-core", version = "0.6.0" }`.
- Prove the current MVP can compile against p2panda `0.6.0` APIs without using
  RC iroh. `p2panda-net 0.6.0` brings `iroh 0.98.2`, which is acceptable.
- Build tiny tests or examples for:
  - encoding/decoding a Ployz fact as `Operation<PloyzFactExtensions>`;
  - deriving a stable Ployz fact id/content hash from p2panda operation data;
  - publishing or type-checking canonical fact operations through
    `p2panda-net::sync::LogSync`;
  - opening a p2panda-store SQLite store with operation/log/topic support;
  - reducing p2panda-auth group state into a Ployz authority snapshot shape.
- Produce a deletion ledger with `delete now`, `delete after migration`, `keep
  as fixture`, and `keep product-owned` categories.
- Update `MVP/design-notes/p2panda-substitution-audit.md`,
  `MVP/primitive-decisions.md`, and `MVP/e2e-proof-plan.md` with evidence from
  the spike.
- If the compile spike proves a simple whole-workspace upgrade path, write the
  next implementation slice plan immediately after the report.

Out of scope:

- No product command behavior changes.
- No deploy, ACME, machine, volume, environment, gateway, or DNS behavior
  migration in this slice.
- No feature reduction. Existing stress and E2E proof scope stays intact.
- No p2panda-blobs adoption unless the spike unexpectedly proves it compiles
  cleanly with the `0.6.0` net/store line and removes local payload code.
- No new Ployz bus semantics. Request/reply, queue groups, services, bridge
  imports/exports, subject grants, and no-responder behavior remain Ployz-owned.
- No quorum, consensus, active-partition write blocking, or witness-ack
  resurrection. Operator-connected node remains the consistency boundary.
- No migration outside `MVP/`.

## Investigation Tracks

### Track 1: Version Alignment

Goal: find whether p2panda `0.6.0` can become the single MVP p2panda line.

Steps:

1. Add an isolated compile spike with aliases for `p2panda-core 0.6.0`,
   `p2panda-store 0.6.0`, `p2panda-sync 0.6.0`, `p2panda-net 0.6.0`, and
   `p2panda-auth 0.6.0`.
2. Run `cargo tree -p mvp-p2panda-06-spike` and record the iroh family.
3. Identify API breaks against `0.5.2`: key types, header fields, topic types,
   store trait bounds, sync stream types, and auth processor/store types.
4. Decide whether the next slice should upgrade all MVP p2panda crates in one
   commit or keep the spike as an adapter while migrating one boundary.

Fit criteria:

- The spike compiles without git dependencies.
- The spike uses non-RC `iroh 0.98` through p2panda-net.
- The upgrade plan does not require product crates to import raw p2panda-net
  types outside `mvp-p2panda-transport`.

### Track 2: Canonical Fact Operation

Goal: test whether the Ployz fact can be the p2panda operation directly.

Today the transport path signs a wrapper operation whose body is a Ployz fact
wire envelope. That duplicates protocol work. The replacement candidate is:

```text
p2panda Operation<PloyzFactExtensions>
  header extensions:
    island id
    fact key
    author principal
    authority epoch/frontier
    content hash
    fact kind
  body:
    fact payload bytes
```

The spike should prove:

- extension encoding is stable and explicit;
- operation hash can serve as the durable fact operation id;
- body hash can serve as or derive the existing content hash;
- duplicate operations classify as duplicates without a wrapper hash cache;
- same-key different-content operations remain reducer-visible conflicts;
- malformed, wrong-island, wrong-author, and unauthorized operations fail with
  structured Ployz import statuses.

Fit criteria:

- `PFO1` is not needed on the success path.
- Any remaining envelope is only for exported debug/backward-compatible test
  fixtures, not live p2panda-net transport.
- Existing `FactSource` consumers would not learn p2panda types.

### Track 3: Direct LogSync Into Canonical Store

Goal: see whether `PandaNetQuarantineLog` can disappear.

The current wrapper log exists because p2panda-net carries operations, while
Ployz wanted to validate and import fact envelopes through `PandaFactStore`.
With canonical fact operations, the better shape may be:

```text
p2panda-net LogSync<Topic, Operation<PloyzFactExtensions>>
  -> local p2panda-store receives operations
  -> Ployz import policy classifies candidates
  -> derived indexes rebuild from the same store
```

The spike should type-check enough of this path to know whether
`mvp-p2panda-facts` can implement the needed store traits or wrap
`p2panda-store` without a second topic/log/quarantine store.

Fit criteria:

- Networked fact delivery has one signed p2panda operation, not wrapper plus
  inner operation.
- Replayed live-stream entries rely on canonical operation deduplication, not a
  separate wrapper hash cache.
- Import rejection reporting survives. We still need an operator/audit surface
  for malformed, wrong-island, unauthorized, stale-writer, and oversized
  operations.

### Track 4: Authz Store Upgrade

Goal: determine whether p2panda-auth/store `0.6.0` lets us delete more custom
authz scaffolding.

Slice 035 correctly kept `IslandAuthoritySnapshot` as the Ployz seam. That seam
should survive. The question is whether the underlying membership log and group
store can be less custom with p2panda `0.6.0`.

The spike should test:

- p2panda-auth `0.6.0` group processing over persistent store data;
- root/admin anchoring remains Ployz-owned;
- strong removal and demotion still reduce deterministically;
- `ReplicaImporter` remains a Ployz condition, not broad write authority;
- membership replay can rebuild principal/key/epoch bindings without manual
  trusted-author seeding.

Fit criteria:

- Current `IslandAuthoritySnapshot` callers do not change.
- Any manual trust fallback left after migration is harness-only or explicitly
  temporary in the decision ledger.
- Fact imports still fail closed for removed/demoted writers until the future
  fact-log frontier proof exists.

### Track 5: Proof Path Retirement

Goal: avoid carrying old proof paths forever.

After the compile spike, classify these E2Es:

- keep in `mvp-e2e all`;
- keep but mark as fixture/historical;
- replace with a p2panda `0.6` equivalent;
- delete after the migration slice.

Minimum targets to classify:

- `iroh-docs-contract`;
- `p2panda-net-sync-contract`;
- `p2panda-net-owned-node-contract`;
- `p2panda-net-fact-node-contract`;
- `p2panda-net-process-serving-contract`;
- `process-role-serving-contract`;
- any product canary still seeding manual trusted authors or replicas.

Do not delete tests in this slice unless the replacement proof already exists
and the deletion is mechanical. The default action is to produce the next
slice's deletion list.

## Expected Slice Artifacts

This slice should land as small commits:

1. `plan`: this plan.
2. `spike`: `mvp-p2panda-06-spike` or equivalent compile-only code and tests.
3. `report`: deletion ledger and docs updates.
4. `simplify`: remove spike noise, rename unclear adapters, and keep only the
   evidence needed for maintainers.

Do not fold the simplify pass into the implementation commit. Slice 036 showed
that smaller commits make review cheaper and prevent the implementer from
holding too many invariants in head at once.

## Verification

Target checks for this slice:

```text
cd MVP && cargo fmt --all -- --check
cd MVP && cargo check -p mvp-p2panda-06-spike
cd MVP && cargo test -p mvp-p2panda-06-spike
cd MVP && cargo tree -p mvp-p2panda-06-spike
```

If the spike touches shared workspace dependency versions, also run:

```text
cd MVP && cargo check --workspace
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

If it remains a strictly isolated alias crate, do not spend the full E2E budget
until the follow-up migration slice.

## Review Questions

- Does the spike prove real deletion, or did it add an adapter beside the old
  adapters?
- Does any product crate learn p2panda-net/raw transport types?
- Can `PandaFactWireEnvelope` be removed from live transport without losing
  branchable import errors?
- Can `PandaNetQuarantineLog` be deleted while preserving malformed/oversized/
  unauthorized operation reporting?
- Do p2panda-auth `0.6.0` stores reduce custom authz code, or is Slice 035's
  current seam already the right amount of Ployz ownership?
- Which E2E proofs are now redundant, and which are still carrying unique
  reliability evidence?

## Success Criteria

The slice succeeds only if it produces one of these concrete outcomes:

1. A compile-backed migration plan to upgrade the MVP to p2panda `0.6.0` and
   delete at least one major custom substrate path in the next slice.
2. A compile-backed rejection that names the exact upstream API mismatch and
   records the smallest future condition that would make deletion possible.

An inconclusive "maybe p2panda can help later" is a failure. The whole point of
this slice is to turn the bias toward p2panda into a concrete delete/keep/defer
map.
