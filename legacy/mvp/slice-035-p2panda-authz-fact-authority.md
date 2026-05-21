---
title: Slice 035 p2panda Authz Fact Authority
status: partial
created: 2026-05-18
plan: MVP/slice-035-p2panda-authz-fact-authority-plan.md
---

# Slice 035 p2panda Authz Fact Authority

## Result

`PandaFactStore` now has a product-shaped authority seam:
`IslandAuthoritySnapshot` is installed at open/rebuild time and answers the
membership questions the fact store needs for local writes, replica import, and
sync scopes.

The new path removes manual trust setup from the p2panda fact-source E2E. That
scenario builds authz membership, installs the snapshot into both persistent
stores, imports through a replica principal, reopens the stores, rebuilds
SQLite projections, and verifies gateway/DNS snapshots from the imported facts.

Manual `trusted_author_keys` and `trusted_replica_peers` remain in the fact
store as fallback/fixture seams for islands without an installed snapshot. They
are no longer the product proof path.

## Authority Semantics

The slice intentionally fails closed on removed/demoted writers:

- Active writer authority for new local writes, replica imports, sync-scope
  authors, local rebuild, and local process-to-process fact movement.
- Replica importer authority for principals allowed to ingest fact operations
  from another store/node.

This is narrower than the original plan. Accepting removed or demoted writers
with only an old epoch would let a stale partition forge fresh facts after
removal and have them reappear on rebuild. Cross-node import or local rebuild
of genuinely pre-removal facts needs a future fact-log frontier or cutoff proof.

## Follow-Up

- Move any remaining product callers off manual trusted-author and
  trusted-replica setup.
- Add fact-log frontier evidence before accepting historical removed-writer
  imports from another replica.
- Replicate membership operations through the same process-serving/p2panda-net
  path used for fact operations.

## Verification

```text
cargo fmt --manifest-path MVP/Cargo.toml --all
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-authz --all-targets
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts --all-targets
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport --all-targets
cargo check --manifest-path MVP/Cargo.toml --workspace --all-targets
cargo clippy --manifest-path MVP/Cargo.toml --workspace --all-targets -- -D warnings
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-fact-source-contract
```
