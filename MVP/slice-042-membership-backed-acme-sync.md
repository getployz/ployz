---
title: Slice 042 Membership-backed ACME and Sync
status: completed
created: 2026-05-19
origin:
  - MVP/slice-042-membership-backed-acme-sync-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/semantic-leverage-loc.md
---

# Slice 042 Membership-backed ACME and Sync

Slice 042 moves the ACME HTTP-01 canary and the main p2panda sync proof off
caller-owned trusted-author and trusted-replica setup. Product-shaped E2Es now
derive writers, replica importers, and sync scopes from durable membership
snapshots through `PandaFactAuthoritySource` and
`PandaFactSyncScope::from_authority`.

Manual trust APIs still exist in `mvp-p2panda-facts`, but this slice narrows
their role. ACME and the main sync contract no longer call them. The p2panda-net
fact-node regression still uses a visibly named `manual_fallback_store` fixture
so it can manufacture unauthorized-author and unauthorized-replica probes
without pretending that path is the product authority model.

## What Changed

- `p2panda-acme-http01-contract` now opens left/right SQLite stores with a
  membership-backed authority source.
- The ACME sync scope now comes from an `IslandAuthoritySnapshot`; replica
  import authority is separate from writer authority.
- `p2panda-sync-fact-source-contract` now builds its main persistent and load
  cases from shared membership fixtures instead of manual trust maps.
- The shared p2panda projection fixture exposes authority snapshots and
  authority sources so future E2Es do not repeat local membership plumbing.
- The p2panda-net fact-node regression names its remaining manual trust fixture
  as fallback-only.

## Proofs

Targeted checks run during the slice:

```text
cargo check --manifest-path MVP/Cargo.toml -p mvp-e2e
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-acme-http01-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-sync-fact-source-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-contract
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts
cargo test --manifest-path MVP/Cargo.toml -p mvp-e2e --all-targets
```

The containment grep now has no hits in the ACME or main sync contracts:

```text
rg -n "PandaTrustedAuthorKey|with_trusted_author_key|trust_replica_peer|from_trusted_authors|trusted_authors|trust_author_key" \
  MVP/e2e/src/p2panda_acme_http01_contract.rs \
  MVP/e2e/src/p2panda_sync_fact_source_contract.rs
```

The broader targeted grep has remaining hits only in the p2panda-net regression
fixture, where manual trust is deliberately used to exercise rejection branches.

## Semantic Leverage Ledger

Slice diff from the Slice 042 plan commit:

```text
MVP/e2e/src/p2panda_acme_http01_contract.rs      |  72 ++++-----
MVP/e2e/src/p2panda_net_fact_node_contract.rs    |  65 ++++----
MVP/e2e/src/p2panda_projection_fixture.rs        |  45 +++++-
MVP/e2e/src/p2panda_sync_fact_source_contract.rs | 188 +++++++++++++----------
4 files changed, 218 insertions(+), 152 deletions(-)
```

This is not a raw LOC reduction. The slice adds about 66 net lines in E2E and
fixture code while replacing a second authority idiom with the shared membership
authority shape. That is still the right kind of leverage: future product proofs
should ask the fixture for membership-backed authority instead of hand-building
trusted-author maps, replica-peer sets, and sync scopes.

Current ACME LOC signal remains honest:

```text
Old cert coordination and backend files:     ~1,180 physical Rust lines
MVP acme + acme-command + lease crates:      ~4,151 physical Rust lines
```

ACME has not beaten the old code on size yet. The useful result is semantic:
leases, challenge ownership, p2panda facts, sync, projection rebuild, and
last-good HTTP-01 serving are explicit primitives with branchable failures. The
next ACME work must add less custom substrate than the old code did.

## Follow-up

Remaining manual trust call sites are now easier to classify:

- `MVP/p2panda-facts/src/lib.rs`: fallback API and unit tests.
- `MVP/p2panda-transport/src/tests.rs`: transport test fixture.
- E2E product canaries still using manual setup: deploy restart recovery,
  machine remove, volume transfer, and environment branch/promote/rollback.
- Adapter helpers in machine/commands p2panda crates that should either accept
  membership authority or be deleted when their callers migrate.

The next deletion/simplification slice should avoid broad churn. Pick one
remaining product canary, move it to the shared membership fixture, and keep
manual trust visible only where a low-level rejection probe needs it.
