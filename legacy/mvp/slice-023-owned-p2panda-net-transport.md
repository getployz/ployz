---
title: Slice 023 Owned p2panda-net Transport
status: completed
completed: 2026-05-18
plan: MVP/slice-023-owned-p2panda-net-transport-plan.md
---

# Slice 023 Owned p2panda-net Transport

## What Shipped

The p2panda-net proof now uses Ployz-owned node lifecycle instead of
`p2panda-net::test_utils` on the default E2E path.

- `mvp-p2panda-transport` owns startup for `AddressBook`, `Endpoint`, `Gossip`,
  `LogSync`, explicit bootstrap node info, and bounded stream waits.
- Stable Ployz `PandaFactOperation` envelopes remain the network payload.
- The receive/import driver decodes envelopes and calls
  `PandaFactStore::import_replica_operation`.
- Import outcomes are structured as imported, duplicate, conflict, deferred,
  failed, or rejected.
- `p2panda-net-owned-node-contract` proves the transport/import boundary.
- `p2panda-net-acme-http01-contract` proves a product path over owned transport.

p2panda-net's current store remains quarantine transport state. Projection still
only reads from the canonical `PandaFactStore`.

## Invariants Proven

- Untrusted replica sessions are rejected before normal author/grant checks.
- Untrusted authors and cross-island operations do not leak into candidate
  reads.
- Same-key races remain conflict candidates.
- Malformed envelopes do not poison the stream.
- ACME HTTP-01 can serve from transported facts while the issuer adapter is
  absent.
- SQLite projection can be deleted and rebuilt from transported p2panda facts.

## Metrics

Latest focused `p2panda-net-owned-node-contract` run:

```json
{
  "transported_operations": 7,
  "imported_operations": 3,
  "duplicate_operations": 1,
  "conflict_candidates": 2,
  "unauthorized_replica_rejected": true,
  "untrusted_author_rejected": true,
  "cross_island_rejected": true,
  "malformed_rejected": true,
  "projected_nodes": 1,
  "projection_rebuild_ms": 5,
  "network_sync_ms": 49,
  "elapsed_ms": 109
}
```

Latest focused `p2panda-net-acme-http01-contract` run:

```json
{
  "replayed_operations_before_clear": 3,
  "replayed_operations_after_clear": 5,
  "imported_before_clear": 3,
  "imported_after_clear": 2,
  "duplicate_after_clear": 3,
  "trusted_replica_required": true,
  "projection_reload_ms": 6,
  "http_request_us": 449,
  "command_adapter_outage_serving_success_count": 1,
  "sqlite_rebuild_after_delete": true,
  "http_404_after_clear": true,
  "elapsed_ms": 282
}
```

## Maintenance Read

This slice is an adapter layer, not a net LOC reduction by itself. It earns its
keep only because both the generic network proof and the ACME product canary
now share the same import driver and owned node wrapper. The next honest
maintenance-burden proof should delete or retire old vertical-path code, not
add more parallel substrate.

## Verification

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-owned-node-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-acme-http01-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-sync-contract
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport --all-targets -- -D warnings
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-e2e --all-targets -- -D warnings
```
