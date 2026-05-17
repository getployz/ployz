---
title: Slice 003 Authority Islands
status: completed
completed: 2026-05-17
plan: MVP/slice-003-authority-islands-plan.md
---

# Slice 003 Authority Islands

## Result

This slice added the first authority boundary for the MVP rewrite:

- `BusSession` now carries `IslandId` plus `PrincipalId`.
- `BusMessage` and `ResponseMessage` are island-scoped.
- Publish, subscribe, request/reply, request-many, queue groups, response
  permits, drain, and grant revocation all evaluate grants inside one island.
- Cross-island subscribers and responders are ignored even when they use the
  same subject name.
- Facts are immutable per `(island, key)` in the in-memory contract harness.
- Fact reads and writes require island-scoped allow/deny grants and return
  structured authorization or conflict errors.
- `BusActorHandle` exposes fact reads and writes, so future business logic can
  stay on the actor-facing surface.

The important product proof is now direct: laptop and prod can use the same
subject and fact key without sharing deliveries or truth.

## Crate Decision

No new authorization crate was added.

`cedar-policy` and `biscuit-auth` remain good candidates for later policy or
delegated-token needs, but they would make this slice harder to inspect. The
current product rule is smaller and more concrete: island grants are local data,
deny beats allow for fact writes, and revocation removes a principal's future
authority before dispatch or mutation.

The decision is recorded in
[MVP/primitive-decisions.md](primitive-decisions.md).

## Proof

Targeted checks run:

```text
cd MVP && cargo test -p mvp-bus fact
cd MVP && cargo fmt --all && cargo test -p mvp-bus
cd MVP && cargo run -p mvp-e2e -- authority-contract
cd MVP && cargo run -p mvp-e2e -- scale
```

Results:

- `mvp-bus`: 72 unit tests passed.
- `authority-contract`: passed and wrote
  `MVP/target/mvp-e2e/authority-contract-metrics.json`.
- `scale`: passed and wrote `MVP/target/mvp-e2e/scale-metrics.json`.
- Scale still covers 200, 1,000, and 10,000 logical nodes.
- New multi-island scale case: 1,000 subscribers split across two islands,
  500 deliveries and 500 request-many replies inside the publishing island,
  0 deliveries or request calls in the other island, 0 cross-island deliveries.
- New queue-group scale case: 10,000 queue subscribers, 100 requests, 100
  deliveries, and 100 unique responders.
- Authority contract now also proves queue groups are island-scoped: the same
  queue name and subject can exist in laptop and prod, and a laptop request
  reaches only the laptop queue member.

## Semantic-Leverage Check

Business rule: "laptop cannot write prod facts directly."

After the primitive exists, the product assertion lives in one E2E scenario:

- laptop writes `/facts/deploy/d1/plan` in the laptop island,
- prod already has the same key in the prod island,
- reading prod still returns the prod content hash,
- reading laptop returns the laptop content hash.
- an ungranted prod principal cannot read or write the prod fact.
- zero-max `request_many` still goes through authorization and drain checks,
  including through the actor facade.

That rule did not require a transport branch, a global enum variant, a special
case in dispatch, or SQLite state. Future feature authors should call
`BusActorHandle::write_fact` and `BusActorHandle::read_fact`; they should not
inspect grants manually.

Implementation footprint for this slice:

- Bus primitives: `MVP/bus/src/grants.rs`, `MVP/bus/src/message.rs`,
  `MVP/bus/src/error.rs`, `MVP/bus/src/facts.rs`,
  `MVP/bus/src/memory.rs`, `MVP/bus/src/actor.rs`,
  `MVP/bus/src/lib.rs`.
- E2E proof: `MVP/e2e/src/authority_contract.rs`,
  `MVP/e2e/src/bus_syntax.rs`, `MVP/e2e/src/main.rs`,
  `MVP/e2e/src/scale.rs`.
- Maintainer docs: `MVP/primitive-decisions.md`, `MVP/README.md`,
  this report.

## Follow-Up

- Slice 004 should use this boundary for bridge import/export semantics instead
  of inventing a second authority model.
- Slice 005 can replace the in-memory fact set with iroh-docs-backed facts and
  keep the same fact authorization contract.
- A fuller ADR archive is not needed yet. Keep updating
  `MVP/primitive-decisions.md` per slice until the number of primitive choices
  makes a larger documentation structure worth the cost.
