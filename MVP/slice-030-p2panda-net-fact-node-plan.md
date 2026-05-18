---
title: Slice 030 p2panda-net Fact Node Plan
status: active
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-020-p2panda-sync-fact-replication-plan.md
  - MVP/slice-023-owned-p2panda-net-transport-plan.md
  - MVP/slice-029-shared-p2panda-fact-store-plan.md
  - MVP/p2panda-transport/src/node.rs
  - MVP/p2panda-transport/src/fact_driver.rs
  - MVP/e2e/src/p2panda_net_sync_contract.rs
  - MVP/e2e/src/p2panda_net_owned_node_contract.rs
external:
  - https://docs.rs/p2panda-net/latest/p2panda_net/
  - https://docs.rs/crate/p2panda-net/latest/features
  - https://github.com/p2panda/p2panda
  - https://www.iroh.computer/docs/concepts/endpoint
---

# Slice 030 p2panda-net Fact Node Plan

## Problem Frame

The MVP has two p2panda replication proofs today:

- `sync_panda_fact_stores` proves p2panda-sync semantics between stores in one
  process.
- `mvp-p2panda-transport` proves `p2panda-net` can move opaque operation bodies
  over local iroh endpoints.

That second proof is still too courier-shaped. The E2E builds wire envelopes,
pushes them through `p2panda-net`, collects `Vec<Vec<u8>>`, and then manually
imports those bodies into a separate canonical store. That proves the network
can carry bytes, but not the architecture we actually want: a running node with
its own durable fact store, replica authority, import policy, projection reads,
and explicit sync status.

The user explicitly confirmed it is acceptable to avoid the RC iroh line. That
means this slice should not block on aligning every MVP iroh wrapper. It should
isolate whatever `p2panda-net` needs inside `mvp-p2panda-transport` and prove
the maintained p2panda network stack can replace more of our bespoke
replication plumbing.

## Single Proof Target

Add a `p2panda-net-fact-node-contract` E2E:

1. spawn two local `PandaNetFactNode` instances in the same p2panda network,
2. give each node a local p2panda fact store and explicit replica session,
3. write facts on node A through `SharedPandaFactStore`,
4. have node B ingest operations from its live `p2panda-net` stream directly
   into its own local store,
5. project from node B without manual operation import in the E2E,
6. prove duplicate, conflict, untrusted-author, cross-island, malformed, and
   unauthorized-replica outcomes are surfaced as structured import status,
7. restart node B's projection from its synced store and rebuild snapshots,
8. report bounded sync/projection metrics.

The E2E should no longer call `transport_wire_bodies` followed by
`import_fact_bodies` for the main success path. Those helpers can stay as
lower-level tests, but the product proof must exercise a running fact node.

## Requirements Trace

- `VISION.md`: primitives should be explicit commands over a legible system,
  not hidden controller loops. The fact node must expose import status and
  projection results rather than silently reconciling cluster truth.
- `MVP/overall-plan.md`: reduce business-code plumbing by leaning on the right
  primitives; prefer maintained p2panda networking where it can replace our
  own code.
- `MVP/architecture.md`: the daemon is not the data plane. Fact replication and
  projection should be independent enough that steady-state serving can keep
  consuming local facts/snapshots while the coordinator is absent.
- `MVP/e2e-proof-plan.md`: remaining proof gaps include cross-process or
  network-backed p2panda replication for serving state, plus exact metrics.
- `MVP/primitive-decisions.md`: keep direct author import, trusted replica
  import, and projection read authority distinct.

## Dependency Scout

Checked on 2026-05-18:

- `p2panda-net` 0.5.2 documents `AddressBook`, `Discovery`, `Endpoint`,
  `Gossip`, and `LogSync` as the network modules. Its docs describe the crate
  as data-type agnostic and explicitly intended for applications to bring their
  own payload encoding.
- The crate's default features include address book, discovery, gossip, iroh
  endpoint, mDNS, and sync. That is exactly the part we are currently
  hand-proving with local endpoint wrappers and byte couriers.
- The p2panda repository describes `p2panda-net` as finding peers, connecting
  directly, and exchanging arbitrary byte streams. It also keeps sync and store
  as separate crates, which matches the MVP split between transport and
  `mvp-p2panda-facts`.
- iroh's endpoint docs confirm that endpoints are encrypted peer-to-peer QUIC
  connections with relay-assisted reliability and cheap streams. We do not need
  to force the existing `mvp-iroh` crate to own this in the same slice.

Decision:

- Use `p2panda-net` inside `mvp-p2panda-transport` for this slice.
- It is acceptable for `mvp-p2panda-transport` to depend on p2panda git crates
  and their matching iroh line while the rest of the MVP remains on its current
  stable crate choices.
- Do not add a second manual transport protocol or another fake network
  harness. If `p2panda-net` has rough edges, wrap those edges narrowly and make
  them visible as structured startup/import errors.

## Scope

In scope:

- Add a small `PandaNetFactNode` or equivalently named type in
  `MVP/p2panda-transport/src/`.
- Give it:
  - a `PandaNetNode`,
  - a `SharedPandaFactStore`,
  - a replica `BusSession`,
  - a topic,
  - an import report/status surface,
  - start/stop or task-handle semantics that do not leak tasks after tests.
- Convert one E2E proof to the running-node shape.
- Keep lower-level opaque-body helpers only where they remain useful as unit
  test fixtures.
- Update decision/proof docs with what p2panda-net now owns and what remains
  placeholder.

Out of scope:

- No production daemon wiring.
- No broad p2panda-auth adoption.
- No cross-island bridge replication policy beyond rejecting unauthorized
  operations at import.
- No replacing `mvp-bus` request/reply semantics.
- No changing root workspace dependencies outside `MVP/`.
- No forcing the existing `mvp-iroh` crate to match `p2panda-net`.
- No PhasedCommand slice.

## Design Decisions

### `p2panda-net` Owns Transport, Ployz Owns Authority

`p2panda-net` should discover/connect/sync byte streams. It should not decide
whether a Ployz fact is authorized. The import path remains:

```text
p2panda-net operation body
  -> PandaFactWireEnvelope decode
  -> PandaFactStore::import_replica_operation
  -> trusted replica check
  -> trusted author key check
  -> island/key/read-write policy checks
  -> projection reads local store
```

This keeps the current security boundary intact while deleting more of our
network plumbing.

### A Fact Node Is A Substrate Actor, Not A Coordinator

The fact node may ingest operations and publish import status. It must not
rewrite durable truth, resolve business conflicts, or start product commands.
Reducers and commands still decide what conflicts mean. The node's job is to
make local facts converge and make failed imports visible.

### Keep p2panda Git Dependency Localized

The current `mvp-p2panda-facts` crate uses crates.io p2panda 0.5.2 for the
stable store/sync path and has git crates only for compatibility tests.
`mvp-p2panda-transport` already uses git p2panda crates at a pinned revision.
This slice should keep that isolation: transport may use the p2panda-net stack
it needs, but domain crates should not learn those git crate types.

### Prefer Running-Node Metrics Over Byte Courier Metrics

The important metric is no longer "how long did moving N bodies take?" It is:

- node startup time,
- first operation observed time,
- imported/duplicate/conflict/rejected/deferred/failed counts,
- projection rebuild time from the receiving node's local store,
- whether tasks shut down without orphaned background work.

## Implementation Units

### Unit 1: Fact Node API

Files:

- `MVP/p2panda-transport/src/lib.rs`
- `MVP/p2panda-transport/src/fact_node.rs`
- `MVP/p2panda-transport/src/node.rs`
- `MVP/p2panda-transport/src/errors.rs`
- `MVP/p2panda-transport/src/tests.rs`

Requirements:

- Introduce a running fact-node wrapper around `PandaNetNode`.
- Accept `SharedPandaFactStore` rather than raw `PandaFactStore` so callers do
  not fork another store wrapper.
- Provide an async import loop over one `PandaNetStream`.
- Return or publish a structured import report using existing
  `PandaNetFactImportOutcome` variants.
- Bound stream reads and startup with existing timeout error variants.
- Provide an explicit shutdown path or task handle suitable for E2E cleanup.

Test scenarios:

- two fact nodes sync one valid fact and the receiver's store can project it,
- duplicate operation increments duplicate status without adding a candidate,
- same-key/different-payload operation becomes conflict candidate,
- untrusted author is rejected,
- unauthorized replica session is rejected before ingest,
- malformed body is rejected and does not stop later valid imports,
- import loop exits cleanly on requested shutdown.

### Unit 2: E2E Running-Net Fact Source Contract

Files:

- `MVP/e2e/src/p2panda_net_fact_node_contract.rs`
- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/p2panda_net_sync_contract.rs`
- `MVP/e2e/src/p2panda_net_owned_node_contract.rs`

Requirements:

- Add `p2panda-net-fact-node-contract` to the E2E scenario list.
- Use node A writes plus node B live ingestion as the primary success path.
- Project from node B's local `SharedPandaFactStore`.
- Delete/rebuild node B's projection SQLite and prove the synced local store is
  sufficient.
- Keep existing courier-shaped E2Es only if they still provide distinct
  malformed/edge coverage; otherwise consolidate obvious duplication.

Test scenarios:

- exact projected node/service/serving counts after net replication,
- conflict candidates survive network import,
- cross-island operations do not leak into the receiving prod projection,
- untrusted author and malformed operation are reported without killing the
  import loop,
- repeated operation is reported as duplicate/no-op,
- projection rebuild after deleting SQLite matches the pre-delete state,
- scenario metrics are written with startup/sync/projection timings.

### Unit 3: Docs And Decision Ledger

Files:

- `MVP/primitive-decisions.md`
- `MVP/e2e-proof-plan.md`
- `MVP/overall-plan.md`
- `MVP/slice-030-p2panda-net-fact-node.md`

Requirements:

- Record that `p2panda-net` is now the preferred maintained transport for fact
  replication proofs in MVP.
- Record the non-RC-iroh decision: transport crate may isolate p2panda-net's
  dependency line instead of forcing global iroh alignment.
- Mark the old opaque-body courier helpers as lower-level fixtures, not the
  production-facing proof shape.
- Keep remaining production gaps explicit: process-role wiring, real
  long-lived daemon startup, p2panda-auth, and production relay/discovery
  topology.

## Verification

Run at minimum:

```text
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport --all-targets
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts -p mvp-p2panda-transport
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-sync-contract
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport -p mvp-e2e --all-targets -- -D warnings
MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
```

Review workflow:

- Run a normal code review with subagents after implementation.
- Run a simplify pass separately and land simplification in its own commit if
  it changes code.
- Do not review tiny mechanical fixes separately; fold them into the current
  slice unless they expose a real correctness issue.

## Risks

- `p2panda-net` background components may have startup or shutdown behavior
  that is awkward in deterministic tests. Keep lifecycle errors structured and
  do not hide failures behind sleeps.
- The current transport wrapper uses a quarantine log before importing into the
  real Ployz fact store. The slice should keep that boundary if direct store
  integration would force git p2panda types upward into `mvp-p2panda-facts`.
- A fact-node loop can accidentally become a reconciler. It must only ingest
  and report; product decisions stay in commands and reducers.
- Live-mode ordering can surface out-of-order operations. Preserve the existing
  deferred outcome instead of treating it as corruption.

## Success Criteria

- A running `p2panda-net` fact node receives and imports facts into its own
  local Ployz fact store.
- The main E2E proof no longer manually imports transported bodies after the
  network step.
- Authorization, conflict, duplicate, malformed, and cross-island failures are
  exact structured outcomes.
- Projection rebuild from the receiver's synced local store passes.
- Transport dependency decisions are documented, including the acceptable
  non-RC iroh isolation.
- The full MVP E2E budget remains green.
