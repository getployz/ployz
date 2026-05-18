---
title: Slice 021 P2panda-Backed ACME HTTP-01 Report
status: completed
created: 2026-05-18
plan: MVP/slice-021-p2panda-acme-http01-plan.md
---

# Slice 021 P2panda-Backed ACME HTTP-01 Report

## What Shipped

- Added `p2panda-acme-http01-contract` to `mvp-e2e`.
- Wrote ACME lease claim/release and HTTP-01 present/clear facts through
  `mvp-p2panda-facts`.
- Replicated the facts with `sync_panda_fact_stores`, then projected and served
  from the second p2panda SQLite store.
- Proved last-good HTTP-01 serving while the command adapter is absent.
- Proved scoped ACME grants, trusted replica sessions, same-key stale synced
  candidates, deterministic supersession, no-op repeat sync, visible nodes at
  command decision time, and deleted SQLite rebuild.
- Hardened the p2panda-sync proof so sync events are imported as they stream,
  mixed memory/SQLite store sync is covered, and cross-island reads with the
  wrong session do not leak candidates or payloads.

## Semantic Leverage

The ACME canary now reads as the product sequence:

```text
claim advisory lease
present challenge
sync p2panda facts
project and serve HTTP-01
clear challenge
take over after release
reject stale/conflicting writes
```

The substrate remains generic: p2panda owns signed operations and sync, the bus
owns grants and visible-node evidence, lease/ACME own business rules, and
projection/serving own last-good state. No ACME-specific replication path was
added.

The old-code comparison target remains
`crates/ployz-cert-backends` plus
`crates/ployzd/src/daemon/cert_coordination.rs`. Placeholder Hyper serving is
not part of the semantic-leverage comparison because it is a wire proof, not the
surviving production gateway shape.

## Verification

Passed during the slice:

```text
cargo fmt --all
cargo clippy -p mvp-e2e --all-targets -- -D warnings
cargo run -p mvp-e2e -- p2panda-acme-http01-contract
cargo run -p mvp-e2e -- p2panda-sync-fact-source-contract
```

Representative ACME metrics:

```text
visible_nodes_at_decision: 2
command_adapter_outage_serving_success_count: 2
trusted_replica_required: true
duplicate_sync_noop: true
stale_sync_preserved_winner: true
sqlite_rebuild_after_delete: true
http_404_after_clear: true
superseded_count: 5
```

## Follow-Up

The next slice should investigate the largest safe p2panda-net/current-p2panda
API substitution now that deploy and ACME both prove business semantics over
p2panda facts. The bias should be toward deleting Ployz-maintained substrate
code, even if that means avoiding direct rc iroh APIs in favor of p2panda-net's
maintained transport/sync shape.
