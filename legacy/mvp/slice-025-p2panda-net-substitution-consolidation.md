---
title: Slice 025 p2panda-net Substitution Consolidation
status: completed
created: 2026-05-18
---

# Slice 025 p2panda-net Substitution Consolidation

Slice 025 consolidated git p2panda-net usage behind `mvp-p2panda-transport`.
Product E2Es now ask the transport crate to move stable Ployz fact-operation
envelopes across owned p2panda-net nodes; they no longer script p2panda topics,
signing keys, node setup, stream subscription, or sync events directly.

## Deletion Gates

Met:

- `MVP/e2e/Cargo.toml` no longer depends directly on git `p2panda-core`,
  `p2panda-net`, `p2panda-store`, or `p2panda-sync`.
- `MVP/e2e/src/p2panda_acme_http01_contract.rs` no longer has a local
  p2panda-net node harness.
- `MVP/e2e/src/p2panda_net_owned_node_contract.rs` and
  `MVP/e2e/src/p2panda_net_sync_contract.rs` keep scenario assertions but use
  the shared transport helper for wire movement.
- p2panda-net proof helpers live behind the `mvp-p2panda-transport` `harness`
  feature and `harness` module. The production-shaped root API stays focused on
  node lifecycle, streams, typed transport identities, and canonical import.
- The transport wrapper now advertises the actual socket bound by the
  underlying iroh endpoint instead of pre-probing localhost ports. Parallel E2E
  runs should not race on a port chosen before endpoint startup.
- `MVP/p2panda-spike` was deleted after mapping its three behaviors to
  `mvp-p2panda-facts` coverage: signed operation candidates, conflict
  candidates, payload reads by content hash, plus the newer persistence/import
  tests.

## Semantic Leverage

This was not a product-command slice, so the right measurement is maintenance
surface removed:

- E2E direct git p2panda dependencies: 4 dependency entries removed.
- E2E direct git p2panda imports: removed from all p2panda-net canaries.
- E2E source delta across the three affected p2panda-net scenarios: 161 added,
  363 removed.
- Shared transport crate delta, including the feature-gated harness module: 308
  added, 68 removed.
- Obsolete spike crate removed: 411 lines.
- Total implementation diff before docs: 470 added, 848 removed.

The useful shape is that p2panda-net remains a carrier/quarantine boundary.
Received bytes still decode as stable `PandaFactWireEnvelope` values and enter
`PandaFactStore::import_replica_operation`; product reducers and command
semantics stay Ployz-owned.

## Verification

Passed:

```bash
cargo check --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport -p mvp-e2e
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport --features harness
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-sync-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-owned-node-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-acme-http01-contract
cargo check --manifest-path MVP/Cargo.toml --workspace
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport -p mvp-e2e --all-targets -- -D warnings
MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
```
