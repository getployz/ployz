---
title: Slice 032 p2panda-net crates.io Substitution Report
status: implemented
created: 2026-05-18
origin:
  - MVP/slice-032-p2panda-net-crates-io-substitution-plan.md
  - MVP/primitive-decisions.md
  - MVP/overall-plan.md
  - MVP/e2e-proof-plan.md
---

# Slice 032 p2panda-net crates.io Substitution Report

## Result

`mvp-p2panda-transport` no longer depends on git-pinned p2panda crates. The
transport wrapper now uses crates.io `p2panda-net 0.5.2`, `p2panda-core 0.5.2`,
`p2panda-store 0.5.2`, and `p2panda-sync 0.5.2`.

No git p2panda dependency remains in `MVP/Cargo.lock`.

## Compatibility Choices

The crates.io p2panda-net line depends on iroh `0.96`. Instead of forcing RC
iroh, `mvp-iroh` now aligns its direct dependencies with the compatible iroh
family:

- `iroh 0.96.1`
- `iroh-gossip 0.96.0`
- `iroh-docs 0.96.0`
- `iroh-blobs 0.98.0`

The only code change required in `mvp-iroh` was switching the memory endpoint
constructor from the newer preset-based API to `Endpoint::bind()`.

The lockfile still contains an older transitive `iroh-base 0.95` family through
`iroh-docs 0.96.0`/`iroh-tickets 0.2.0`. That is not a direct MVP API line, but
it is part of the dependency graph until p2panda/iroh publish a newer
compatible crates.io set.

The stable p2panda-net API also moved topic and identity names away from the git
line:

- `Topic` became `TopicId`.
- `SigningKey`/`VerifyingKey` became `PrivateKey`/`PublicKey`.
- `LogSync` uses a caller-supplied `TopicMap`.
- `NodeInfo` tickets are encoded through `EndpointAddr`, because stable
  `NodeInfo` is not directly CBOR-serializable.

## Replay Suppression

The process-serving E2E exposed one real behavioral difference: after idle
stream refresh, crates.io p2panda-net can replay an already-seen wrapper
operation. That is acceptable transport behavior, but it should not inflate
Ployz rejection metrics.

`PandaNetFactNode` now records a bounded cache of p2panda wrapper operation
hashes and skips those wrappers when a refreshed stream replays them. This only
suppresses transport replay. Distinct p2panda operations carrying the same
Ployz fact envelope still reach `SharedPandaFactStore` and are classified by
the canonical fact store as duplicate/conflict/rejected.

## Leverage

Before this slice, the transport path had one crates.io p2panda line for
canonical facts and one git p2panda line for network transport.

After this slice:

- git p2panda dependency count: 0
- product/domain crates exposed to raw p2panda-net types: 0
- product E2E assertions weakened: 0
- transport wrapper LOC for the touched runtime files:
  - before: 1,032 lines across `node.rs`, `quarantine_log.rs`, `fact_node.rs`,
    and `harness.rs`
  - after: 1,162 lines across the same files
- maintained Ployz transport wrapper remains narrow: node config, topic config,
  fact-node import, replay suppression, and structured import outcomes

This slice is a dependency-burden reduction, not a LOC win in the transport
wrapper. The registry API needs a caller-supplied topic map and explicit replay
suppression, so the wrapper grew by 130 lines while deleting 113 lines of
obsolete git-compatibility scout tests from `mvp-p2panda-facts`.

The slice removed the dev/test proof that git p2panda store headers were
incompatible with the stable import path. That proof is obsolete now that the
transport and fact-store lines both use crates.io p2panda `0.5.2`.

## Verification

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts --all-targets`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport --all-targets`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-iroh --all-targets`
- `cargo clippy --manifest-path MVP/Cargo.toml --workspace --all-targets -- -D warnings`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-process-serving-contract`
- `MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all`
The final full E2E run completed the suite under the 120s budget.
