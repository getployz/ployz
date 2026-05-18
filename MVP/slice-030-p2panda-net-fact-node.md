---
title: Slice 030 p2panda-net Fact Node
status: completed
completed: 2026-05-18
plan: MVP/slice-030-p2panda-net-fact-node-plan.md
---

# Slice 030 p2panda-net Fact Node

## Result

`mvp-p2panda-transport` now exposes `PandaNetFactNode`, a running node wrapper
that combines:

- an owned `PandaNetNode`,
- a live `PandaNetStream`,
- a local `SharedPandaFactStore`,
- an explicit trusted replica session,
- structured import outcomes for inserted, duplicate, conflict, deferred,
  rejected, and failed imports.

The product proof is `p2panda-net-fact-node-contract`. The sender writes facts
through its local `SharedPandaFactStore` and publishes the resulting stable
Ployz fact envelopes over p2panda-net. The receiver ingests directly from its
live p2panda-net stream into its own local store. Projection reads that receiver
store directly; the E2E no longer collects network bodies and manually imports
them for the main success path.

## Proven Behavior

- Valid same-island operations import into the receiver's store.
- Replayed operation bodies are duplicate no-ops.
- Same-key/different-payload operations remain conflict candidates.
- Untrusted author, cross-island, unauthorized replica, malformed envelope,
  oversized envelope, and pending-queue-full cases are surfaced as structured
  import outcomes.
- Out-of-order operation bodies are deferred, retried when predecessors arrive,
  and retried through transitive chains until no progress remains.
- Cross-island operations do not leak into the prod projection.
- Deleting/rebuilding projection output from the receiver's synced store
  produces the same projected node state.
- The fact node enforces its configured body-size limit at the p2panda stream
  event boundary before converting the operation body into local bytes. This is
  still after p2panda-net receives the operation; production ingress limits
  remain a transport-topology concern.

Latest local `p2panda-net-fact-node-contract` metrics:

```json
{
  "attempted_imports": 11,
  "imported_operations": 6,
  "duplicate_operations": 1,
  "conflict_operations": 1,
  "rejected_operations": 3,
  "conflict_candidates": 2,
  "unauthorized_replica_rejected": true,
  "untrusted_author_rejected": true,
  "cross_island_rejected": true,
  "malformed_rejected": true,
  "no_cross_island_leakage": true,
  "projected_nodes": 1,
  "projected_services": 1,
  "projected_gateway_routes": 1,
  "projected_dns_records": 1,
  "startup_ms": 29,
  "sync_import_ms": 28,
  "projection_rebuild_ms": 3,
  "restart_projection_rebuild_ms": 5
}
```

## Decisions

`p2panda-net` owns transport only. Ployz authority still lives in
`mvp-p2panda-facts`: trusted replica session, trusted author key, original
writer grants, island match, and conflict-as-candidate semantics.

The git p2panda-net dependency and its compatible iroh line remain isolated in
`mvp-p2panda-transport`. Domain crates continue to use stable Ployz contracts:
`FactSource`, `SharedPandaFactStore`, and projection reducers.

Courier-shaped helpers under the transport harness remain lower-level fixtures.
They are still useful for malformed/body-level tests, but product proofs should
prefer the running fact-node shape.

## Verification

Passed:

```text
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport --all-targets
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts -p mvp-p2panda-transport
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-sync-contract
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport -p mvp-e2e --all-targets -- -D warnings
MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
```
