---
title: Slice 005 Fact Projection And Snapshots
status: completed
plan: MVP/slice-005-fact-projection-plan.md
completed: 2026-05-17
---

# Slice 005 Fact Projection And Snapshots

## Result

This slice adds the MVP-local truth-to-view pipeline:

- facts now carry payload bytes and BLAKE3-derived content hashes,
- fact listing and payload reads are island-scoped and grant-filtered,
- fact payload storage is bound to the authorized fact identity so hash-only
  facts cannot read another identity's payload bytes,
- projection payload identity must match the authorized fact key, so a writer
  cannot project `node-b` by writing a forged payload under a `node-a` key,
- `mvp-projection` defines a narrow `FactSource` contract that an iroh-docs
  adapter can implement later,
- typed node, service, route, gateway, and DNS facts reduce deterministically
  into `ProjectionState`,
- SQLite stores rebuildable query tables only,
- SQLite promotion is staged until gateway/DNS snapshots publish successfully,
- `gateway.snapshot` and `dns.snapshot` are atomically replaced JSON files with
  batch rollback,
- snapshot loaders validate schema, island, and symlink targets before
  accepting a file,
- `ProjectionActor` owns one island's projection path, snapshot paths, and
  structured last-success/last-failure status,
- E2E proof covers SQLite deletion rebuild, dropped notification catch-up,
  corrupt snapshot rejection, authorization failures, conflicts, and redacted
  ignored-fact status,
- scale proof projects 200, 1,000, and 10,000 logical nodes through node
  principals, grants, node facts, service facts, gateway route facts, DNS facts,
  SQLite, and snapshot output.

This is `E2E-4a`, not full `E2E-4`. Real iroh-docs anti-entropy, propagation
metrics, and remote service-registry projection remain for the docs-backed
adapter slice.

## Crate Decisions

`rusqlite` is used directly for the disposable projection store. There is no ORM
or repository layer because the whole point is to rebuild a small set of local
query tables from facts.

`tempfile` is used for same-directory temporary snapshot files followed by
atomic persist. Snapshot writes never stream partial bytes over the last good
file.

`blake3` is used for fact payload hashes. This gives the in-memory fact harness
the same content-addressed shape expected from iroh-blobs and iroh-docs.

`thiserror` is used for projection errors so foreground callers can branch on
structured variants without hand-written error boilerplate.

`iroh-docs` is still required for the final architecture, but it is not added in
this slice. Current `iroh-docs 0.99.0` reports Rust 1.91 while this repo and
`MVP/` currently declare Rust 1.88. The adapter/toolchain decision should be a
dedicated slice so reducers do not depend on a temporary compatibility choice.

The maintainer rationale is recorded in
[MVP/primitive-decisions.md](primitive-decisions.md).

## Proof

Checks run for this slice:

```text
cd MVP && cargo test -p mvp-projection
cd MVP && cargo run -p mvp-e2e -- projection-contract
cd MVP && cargo run -p mvp-e2e -- scale
cd MVP && just test
```

Results from the current local run:

- `mvp-bus`: 103 unit tests passed.
- `mvp-projection`: 30 unit tests passed.
- `projection-contract`: passed and wrote
  `MVP/target/mvp-e2e/projection-contract/projection-contract-metrics.json`.
- `scale`: passed and wrote `MVP/target/mvp-e2e/scale-metrics.json`.
- `just test`: passed fmt, clippy, unit tests, and `mvp-e2e all`.

Observed projection-contract metrics:

```text
projected nodes: 1
projected services: 2
gateway routes: 1
dns records: 1
ignored unauthorized: 1
ignored unverified: 1
ignored conflicts: 1
gateway snapshot bytes: 337
dns snapshot bytes: 178
sqlite rebuild after delete: true
dropped hint caught up: true
corrupt snapshot rejected: true
corrupt sqlite rebuilt: true
unauthorized write rejected: true
conflict write rejected: true
payload/key mismatch rejected: true
```

Observed projection scale metrics:

```text
200 logical nodes:
  fact writes: 403
  projected nodes/services: 200/200
  gateway backends: 200
  actor duration: 8ms

1,000 logical nodes:
  fact writes: 2003
  projected nodes/services: 1000/1000
  gateway backends: 1000
  actor duration: 80ms

10,000 logical nodes:
  fact writes: 20003
  projected nodes/services: 10000/10000
  gateway backends: 10000
  actor duration: 445ms
  sqlite bytes: 1585152
  gateway snapshot bytes: 482279
```

## Semantic-Leverage Check

Business rule: "a durable route/gateway/DNS commit becomes the serving view,
and the view can be rebuilt from facts if local query state disappears."

The E2E code expresses that as:

- write typed route, gateway, DNS, node, and service facts,
- call `ProjectionActorHandle::project_once`,
- assert SQLite, `gateway.snapshot`, and `dns.snapshot`,
- delete `projections.sqlite`,
- call `project_once` again,
- assert the same projection and snapshot bytes,
- write another service fact without sending a hint,
- call `project_once` and assert catch-up.

There is no mutable SQL head, manual gap repair, global event sequence, or
gateway-specific shortcut in the business path.

## Follow-Up

- Implement an iroh-docs adapter behind `FactSource` and prove remote
  anti-entropy propagation. That closes `E2E-4b`, not this slice.
- Wire gateway/DNS process roles under `MVP/` to load snapshots and preserve
  last-good in memory across daemon restarts.
- Use projected route commits in the deploy commit-before-drain slice.
- Decide whether `MVP/` raises Rust to 1.91 for current iroh crates or pins an
  older compatible iroh-docs line temporarily.
