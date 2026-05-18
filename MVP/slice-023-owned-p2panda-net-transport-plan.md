---
title: Slice 023 Owned p2panda-net Transport Plan
status: active
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-020-p2panda-sync-fact-replication-plan.md
  - MVP/slice-021-p2panda-acme-http01-plan.md
  - MVP/slice-022-p2panda-net-current-api-substitution-plan.md
  - MVP/slice-022-p2panda-net-current-api-substitution.md
external:
  - https://docs.rs/p2panda-net/latest/p2panda_net/
  - https://docs.rs/p2panda-sync/latest/p2panda_sync/
  - https://docs.rs/tokio-util/latest/tokio_util/
  - https://github.com/p2panda/p2panda
---

# Slice 023 Owned p2panda-net Transport Plan

## Problem Frame

Slice 022 proved the authority shape:

```text
p2panda-net carries opaque Ployz fact-operation envelopes
  -> receiver decodes envelope
  -> PandaFactStore::import_replica_operation
  -> projection-visible facts only after Ployz validation
```

That proof still used `p2panda-net::test_utils::TestNode`. The next slice needs
to remove that false boundary. The MVP should own node lifecycle, bootstrap
addresses, shutdown, bounded waits, and import error surfaces while still
leaning on p2panda-net for endpoint/discovery/gossip/log-sync plumbing.

This is not a rewrite of the fact store. It is a production-shaped transport
adapter around the existing canonical import path.

## Requirements Trace

- `VISION.md`: the operator's connected node is the consistency boundary;
  writes commit locally and replicate eventually. Transport failure must be
  visible, not converted into hidden quorum behavior.
- `MVP/overall-plan.md`: after Slice 022, the next proof should replace
  `p2panda-net::test_utils` with owned node lifecycle, discovery, shutdown, and
  error surfaces.
- `MVP/architecture.md`: p2panda-net is a carrier, not durable cluster truth.
  Network-carried facts must enter through a trusted same-island replica import
  gate before normal fact validation.
- `MVP/e2e-proof-plan.md`: E2E-4 still needs production-owned p2panda-net
  transport, and E2E-7 still needs p2panda cross-node serving replication beyond
  local harnesses.
- `MVP/primitive-decisions.md`: Slice 022 keeps `sync_panda_fact_stores` as
  deterministic same-process proof plumbing and establishes p2panda-net as the
  future carrier.
- User direction: bias toward using maintained p2panda/p2panda-net crates
  instead of maintaining AI-written transport plumbing.

## Scope

In scope:

- Add an MVP-local p2panda-net transport crate or module that uses normal
  p2panda-net APIs, not `p2panda-net::test_utils`.
- Promote the Ployz operation envelope from harness-only proof support to the
  narrow production-facing transport codec required by that crate.
- Build owned startup/shutdown around `AddressBook`, `Endpoint`, `Gossip`, and
  `LogSync`.
- Use explicit bootstrap node information and address-book population as the
  first production-shaped discovery surface. Random-walk discovery and mDNS can
  stay deferred unless they fall out trivially.
- Transport stable `PandaFactOperation` envelopes in current p2panda-net log
  operation bodies, then import them with
  `PandaFactStore::import_replica_operation`.
- Add an owned-node E2E contract that proves duplicate, conflict, untrusted
  author, untrusted replica, cross-island, malformed envelope, shutdown, and
  bounded timeout behavior.
- Add one product canary over the owned transport: ACME HTTP-01 facts sync from
  issuer node to serving/projection node over owned p2panda-net transport, then
  HTTP-01 serves and clears from projected state while the command adapter is
  absent.
- Keep all work inside `MVP/`.
- Keep `MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all` green.

Out of scope:

- Replacing `PandaFactStore` with the current git p2panda store API as
  canonical truth.
- Using p2panda-net's pre-authorized store as projection input.
- Replacing PloyzBus.
- p2panda-auth membership or revocation.
- Random-walk discovery, LAN mDNS, relay deployment, or multi-host testing.
- Hard consensus, quorum, witness acknowledgements, strict leases, or hidden
  active-partition checks.
- Rewriting deploy or machine-remove business logic.
- Replacing HTTP/DNS placeholder wire crates with Pingora or `hickory-server`.

## Crate Scout

Checked before planning:

- `p2panda-net` exposes production builders for `AddressBook`, `Endpoint`,
  `Gossip`, and `LogSync`; `LogSync::stream(topic, live_mode)` returns a
  `SyncHandle` whose subscription yields `TopicLogSyncEvent` values. This is
  enough to build our own node wrapper without `test_utils`.
- `p2panda-net` `Endpoint` supports normal ALPN handler registration and
  connection attempts, but this slice only needs its log-sync/gossip path.
- `p2panda-net` `AddressBook::insert_node_info` accepts trusted bootstrap node
  information, which matches the MVP invite/bootstrap model better than
  building discovery machinery first.
- `p2panda-net` manual sync-session initiation is `#[cfg(test)]`; the owned
  proof must rely on address-book bootstrap plus gossip/log-sync session flow,
  not that private test hook.
- `p2panda-net` has a supervisor feature, but the first owned wrapper should
  use explicit start/drop/shutdown status before adopting its restart
  supervisor. The MVP already wants visible failure surfaces rather than hidden
  background self-healing.
- `tokio-util` provides maintained cancellation/shutdown utilities. Use it for
  wrapper-owned receive loops instead of inventing ad hoc atomic shutdown flags.
- Current git `p2panda-store` APIs are still the quarantine transport store for
  p2panda-net operations. Stable `mvp-p2panda-facts` remains canonical.

Decision:

- Create a thin Ployz-owned p2panda-net transport adapter.
- Copy only the minimal current-operation creation/association logic that
  p2panda-net requires for its quarantine store; do not copy its node lifecycle
  from `test_utils`.
- Keep business facts, reducers, projection, lease semantics, and ACME
  ownership outside the transport crate.

## Design Decisions

### One Canonical Import Gate

The transport adapter must not expose "received fact candidates" directly.
Its receive path is:

```text
TopicLogSyncEvent::OperationReceived
  -> read operation body
  -> decode Ployz fact transport envelope
  -> import_replica_operation(replica_session, operation)
  -> structured Imported / Duplicate / Conflict / Rejected outcome
```

The lower-level `import_operation` remains useful for local deterministic
harnesses. Network transport should call `import_replica_operation`.

### p2panda-net Store Is Quarantine State

p2panda-net needs a current p2panda store so it can publish/sync operations.
That store is a transport log. It is not a `FactSource`, and projection must not
read from it.

The only projection input remains `PandaFactStore`.

### Bootstrap First, Discovery Later

The first proof should use explicit bootstrap node information:

```text
node A exports NodeInfo
node B inserts bootstrap NodeInfo into AddressBook
node B starts Endpoint/Gossip/LogSync
both nodes stream the island topic
```

That maps to the MVP invite/join story and avoids testing mDNS/random-walk
timing before the product needs it.

### Shutdown Is Part Of The Contract

The owned wrapper should expose a bounded shutdown or drop contract:

- stop/pause receive loops with a cancellation token,
- close stream handles by dropping them,
- drop p2panda-net actors in an owned order,
- return visible shutdown/health status for tests.

If p2panda-net actors panic or startup fails, the wrapper returns structured
startup/sync errors. It must not `unwrap` like `test_utils`.

### Product Canary Must Use The Transport

The slice is not complete if it only adds another generic transport scenario.
At least one business canary must use owned p2panda-net transport. ACME HTTP-01
is the right first canary because Slice 021 already has the same fact shape over
same-process `sync_panda_fact_stores`.

## Implementation Units

### Unit 1: Transport Envelope API

Files:

- `MVP/p2panda-facts/src/lib.rs`
- `MVP/p2panda-facts/Cargo.toml`
- `MVP/e2e/src/p2panda_net_sync_contract.rs`

Work:

- Move the current `PandaFactWireEnvelope` codec out of the harness-only module
  into a narrow production-facing transport surface.
- Keep `PandaFactOperation` internals private. Expose encode/decode through one
  codec type, not raw header/body getters.
- Preserve structured decode errors.
- Update the Slice 022 E2E proof to use the new production codec without the
  `harness` feature.

Execution note:

- Contract-first. Compile the E2E binary without `mvp-p2panda-facts/harness`
  before moving on.

Test scenarios:

- Encoding then decoding an exported operation round trips exactly.
- Bad magic, too-short, oversize header length, and missing body produce
  structured errors.
- Existing `p2panda-net-sync-contract` still passes without enabling the
  harness feature on `mvp-p2panda-facts`.

Verification:

- `cargo test -p mvp-p2panda-facts --lib`
- `cargo check -p mvp-e2e`
- `cargo run -p mvp-e2e -- p2panda-net-sync-contract`

### Unit 2: Owned p2panda-net Node Wrapper

Files:

- `MVP/Cargo.toml`
- `MVP/p2panda-transport/Cargo.toml`
- `MVP/p2panda-transport/src/lib.rs`
- `MVP/p2panda-transport/src/node.rs`
- `MVP/p2panda-transport/src/quarantine_log.rs`
- `MVP/p2panda-transport/src/errors.rs`
- `MVP/p2panda-transport/src/tests.rs`

Work:

- Add a small `mvp-p2panda-transport` crate.
- Define typed config and status shapes:
  - node signing key / node id,
  - network id,
  - bind config for local tests,
  - island topic,
  - bootstrap node info,
  - startup/shutdown status.
- Build p2panda-net nodes from normal APIs:
  - `AddressBook::builder().spawn()`,
  - `AddressBook::insert_node_info(...)`,
  - `Endpoint::builder(address_book).spawn()`,
  - `Gossip::builder(address_book, endpoint).spawn()`,
  - `LogSync::builder(quarantine_store, endpoint, gossip).spawn()`.
- Implement a minimal quarantine log client that creates current p2panda
  operations whose bodies are Ployz transport envelopes, inserts them into the
  current p2panda store, and associates them with the island topic.
- Avoid `p2panda-net::test_utils`.
- Wrap startup, publish, subscribe, and shutdown with structured errors and
  explicit timeouts.

Execution note:

- Characterization-first. Build a crate-local test with two owned nodes before
  integrating with `PandaFactStore`.

Test scenarios:

- Two owned local nodes can start with explicit bootstrap info and open the same
  topic stream.
- Node A publishes one opaque payload through its quarantine store, Node B
  receives it as a p2panda-net operation body.
- Duplicate delivery is observable but bounded by the caller's import idempotency.
- Startup failure, stream open failure, publish failure, and timeout have
  structured error variants.
- Dropping/shutting down a node stops wrapper-owned receive loops without
  hanging tests.

Verification:

- `cargo test -p mvp-p2panda-transport`

### Unit 3: Canonical Fact Import Driver

Files:

- `MVP/p2panda-transport/src/fact_driver.rs`
- `MVP/p2panda-transport/src/lib.rs`
- `MVP/p2panda-facts/src/lib.rs`
- `MVP/e2e/src/p2panda_net_owned_node_contract.rs`
- `MVP/e2e/src/main.rs`

Work:

- Add a fact-import driver that subscribes to the p2panda-net topic, decodes
  Ployz envelopes, and imports them into a target `PandaFactStore` with
  `import_replica_operation`.
- Report structured per-operation outcomes:
  - imported,
  - duplicate,
  - conflict,
  - malformed envelope rejected,
  - untrusted author rejected,
  - untrusted replica rejected,
  - cross-island rejected.
- Do not let malformed or unauthorized operations stop the driver unless the
  p2panda-net stream itself fails.
- Add an E2E contract named `p2panda-net-owned-node-contract` that replaces the
  Slice 022 `test_utils` proof with the owned wrapper.

Execution note:

- Authority-first. The driver is not allowed to expose decoded facts before
  import succeeds.

Test scenarios:

- Node A writes a signed p2panda fact, publishes its envelope through the owned
  transport, and Node B imports it through trusted-replica import.
- Repeated delivery is a duplicate no-op.
- Same-key race remains two conflict candidates.
- Untrusted author key is rejected.
- Untrusted replica session is rejected before author/grant checks.
- Cross-island operation is rejected and does not leak to candidate reads.
- Malformed envelope is counted as rejected and does not poison the stream.
- Projection on Node B sees only the valid non-conflicting fact.
- All waits have deadlines.

Verification:

- `cargo run -p mvp-e2e -- p2panda-net-owned-node-contract`
- `cargo clippy -p mvp-p2panda-transport -p mvp-p2panda-facts -p mvp-e2e --all-targets -- -D warnings`

### Unit 4: ACME HTTP-01 Over Owned p2panda-net Transport

Files:

- `MVP/e2e/src/p2panda_net_acme_http01_contract.rs`
- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/p2panda_acme_http01_contract.rs`
- `MVP/e2e/src/p2panda_projection_fixture.rs`

Work:

- Add a product canary named `p2panda-net-acme-http01-contract`.
- Reuse the ACME writer/projection/serving shape from
  `p2panda-acme-http01-contract`, but replace `sync_panda_fact_stores` with the
  owned p2panda-net transport and import driver.
- Keep the existing deterministic same-process ACME sync scenario. It remains
  useful for faster failure injection and should not be deleted in this slice.
- Prove the command/issuer adapter can be absent while serving continues from
  projected last-good state.
- Prove clear/takeover flows replicate over owned transport and do not roll
  serving state backward.

Execution note:

- Product-proof-first. Do not broaden the transport API unless the ACME canary
  needs it.

Test scenarios:

- Issuer node writes lease/challenge facts, transport carries them to serving
  node, projection reloads HTTP-01 state, and wire serving answers the challenge.
- Command adapter is dropped; HTTP-01 continues serving last-good state.
- Clear fact replicates and HTTP-01 returns 404 after projection reload.
- Stale/lower-epoch challenge facts transported later are superseded and cannot
  roll serving back.
- Projection SQLite deletion on serving node rebuilds from the transported
  p2panda store.
- Trusted replica gating and scoped ACME grants remain enforced.

Verification:

- `cargo run -p mvp-e2e -- p2panda-net-acme-http01-contract`
- `cargo run -p mvp-e2e -- p2panda-acme-http01-contract`

### Unit 5: Metrics, Scale Budget, And Documentation

Files:

- `MVP/e2e-proof-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/overall-plan.md`
- `MVP/architecture.md`
- `MVP/slice-023-owned-p2panda-net-transport.md`

Work:

- Record the owned-transport decision in the primitive ledger.
- Update E2E-4 and E2E-7 proof status.
- Record p2panda-net startup/sync/import/shutdown metrics.
- Record whether `test_utils` remains anywhere on the default E2E path and why.
- Compare LOC/maintenance burden against Slice 022:
  - code moved out of E2E,
  - code added in reusable transport crate,
  - deterministic sync helpers retained,
  - business canary code added or simplified.
- Keep the full E2E all-run under the existing 120s budget.

Execution note:

- Closeout docs must state whether this slice is a real deletion/simplification
  or a necessary adapter layer.

Test scenarios:

- Default all-run includes the owned-node transport scenario and ACME canary
  only if they are bounded and deterministic.
- Metrics include network sync duration, import outcome counts, malformed
  rejection count, shutdown duration, and projection reload duration.

Verification:

- `MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all`

## Review Risks

- Accidentally making p2panda-net's current store canonical truth.
- Reintroducing untrusted network import by calling `import_operation` directly
  instead of `import_replica_operation`.
- Smuggling hidden quorum or "active partition" checks into transport health.
- Recreating p2panda-net `test_utils` instead of extracting the minimal normal
  API wrapper.
- Building a broad transport framework before one product canary uses it.
- Adding unbounded live-mode waits that make `mvp-e2e -- all` flaky.
- Treating p2panda-net actor drop as a sufficient production shutdown story
  without wrapper-owned cancellation/status for receive loops.

## Verification Summary

Required before closing the slice:

```text
cargo fmt --all
cargo test -p mvp-p2panda-facts --lib
cargo test -p mvp-p2panda-transport
cargo check -p mvp-e2e
cargo run -p mvp-e2e -- p2panda-net-owned-node-contract
cargo run -p mvp-e2e -- p2panda-net-acme-http01-contract
cargo run -p mvp-e2e -- p2panda-net-sync-contract
cargo run -p mvp-e2e -- p2panda-acme-http01-contract
cargo clippy -p mvp-p2panda-transport -p mvp-p2panda-facts -p mvp-e2e --all-targets -- -D warnings
MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

If owned p2panda-net startup cannot be made deterministic without `test_utils`,
do not fake success. Keep the transport crate out, document the blocker, and
close the slice as a no-go with the exact API gap.
