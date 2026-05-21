---
title: Slice 007 Iroh Toolchain Integration
status: completed
plan: MVP/slice-007-iroh-toolchain-integration-plan.md
completed: 2026-05-17
---

# Slice 007 Iroh Toolchain Integration

## Result

This slice stops deferring iroh and adds the first real docs-backed fact path
under `MVP/`.

The implemented shape is deliberately narrow:

- `MVP/` now declares Rust `1.91` so it can use current iroh crates.
- `mvp-iroh` owns endpoint, router, blobs, gossip, and docs setup.
- Local tests use `Endpoint::bind(presets::Minimal)` so the proof does not
  depend on public relay or DNS availability.
- Docs facts are written through explicit Ployz fact-write authorization.
- Docs authors map to Ployz principals through explicit bindings.
- Unknown docs authors project as `Unverified`, not as trusted principals.
- Revoked or ungranted authors project as `Unauthorized`.
- Conflicting candidates for the same fact key remain visible to projection
  reducers.
- The docs adapter exposes a synchronous local view behind `FactSource`; the
  projection crate and reducers do not import iroh types or await iroh APIs.
- `mvp-e2e` now includes `iroh-docs-contract` in the time-budgeted `all`
  scenario.

This closes the local `E2E-4b` subset: one local iroh-docs fact can sync to a
second local node and feed the same projection contract shape as in-memory
facts. It does not yet prove NAT traversal, relay fallback, machine join,
persistent docs storage, PloyzBus-over-iroh request/reply, or docs-backed bridge
rule replication.

## Crate Decisions

The slice uses the current iroh line:

- `iroh 1.0.0-rc.0`
- `iroh-docs 0.99.0`
- `iroh-blobs 0.101.0`
- `iroh-gossip 0.99.0`

Those crates require a newer compiler than the original MVP-local `1.88`
setting, so only `MVP/Cargo.toml` moves to Rust `1.91`. The root workspace
toolchain and existing codebase are not changed.

`n0-future` is used only to consume iroh-docs streams. It stays inside
`mvp-iroh`; projection code continues to see a synchronous `FactSource`.

The maintainer rationale is recorded in
[MVP/primitive-decisions.md](primitive-decisions.md).

## Proof

Checks run for this slice:

```text
cd MVP && cargo fmt --all --check
cd MVP && cargo test -p mvp-iroh
cd MVP && cargo test -p mvp-bus fact_authorizer_matches_bus_fact_read_and_write_grants
cd MVP && cargo run -p mvp-e2e -- iroh-docs-contract
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

Results from the current local run:

- `mvp-iroh`: 10 unit tests passed.
- `mvp-bus`: 104 unit tests passed.
- `mvp-projection`: 32 unit tests passed.
- `iroh-docs-contract`: passed and wrote
  `MVP/target/mvp-e2e/iroh-docs-contract/iroh-docs-contract-metrics.json`.
- `mvp-e2e all`: passed with `iroh-docs-contract` included.

Observed `iroh-docs-contract` metrics:

```text
sync timeout: 5000ms
initial sync: 89ms
conflict sync: 9ms
unauthorized sync: 8ms
verified candidates: 1
conflict candidates: 2
unauthorized candidates: 1
conflict payloads read: 2
unauthorized payloads read: 0
elapsed: 182ms
```

Observed full E2E wall time:

```text
mvp-e2e all elapsed: 21980ms
```

## Semantic-Leverage Check

Business rule: "a replicated node/service/route fact should be projectable
without business logic learning iroh-docs."

The code now expresses that as:

- write a `FactKey` plus `FactPayload` through `IrohFactDoc::write_fact_payload`,
- sync through iroh-docs,
- refresh the adapter's local view,
- list candidates through `FactSource`,
- read payloads through `FactSource`,
- project with the same reducer vocabulary used by in-memory facts.

Projection reducer files did not need behavior changes. Raw iroh types are
confined to `mvp-iroh` internals and the E2E harness path.

## Review Fixes

- The fact authorizer seam now checks by island/principal/key directly, so a
  docs author can be validated without manufacturing a bus session.
- The E2E runtime enables Tokio IO because even relay-free local iroh endpoints
  open local UDP sockets.
- The iroh-docs E2E creates the second docs author after the initial one-author
  sync proof, then binds it on the imported doc before testing conflicts. That
  keeps the initial proof focused and makes the author-binding rule explicit.
- Same-author rewrites replace the local candidate instead of leaving stale
  candidates behind.
- Payload reads re-check current read/write authority and local metadata, so a
  forged `FactCandidate` cannot make revoked or unverified payloads readable.
- Payload-missing docs entries replace stale payload-bearing entries, so
  projections can see the current candidate without reading an old body.
- Waits that need a fresh docs update now wait for the exact content hash, not
  just the key.
- Malformed docs entries are recorded as rejected entries and skipped, so a bad
  non-Ployz docs key does not block later valid facts in the same refresh.

## Known Limitation

Docs author bindings are explicit bootstrap/test data in this slice. That is
enough to prove the adapter boundary, but it is not the final membership model.
Before machine join or persistent docs access, the MVP needs a replicated
author-binding manifest or equivalent membership fact so already-imported peers
can verify newly authorized docs authors without out-of-band test setup.

## Follow-Up

- ACME singleton primitive, once the operator chooses queue-group singleton,
  lease fact, or named singleton service.
- Deploy commit-before-drain using docs-backed route commit facts and
  `store.pin_fact`.
- PloyzBus-over-iroh request/reply and bridge transport.
- Machine invite/join with iroh endpoint identity and docs access.
- WireGuard full-mesh reconciliation from docs-backed node facts.
- HTTP/DNS serving-state process proof with coordinator down, where
  fact-sync/projection/snapshot application keeps running after the coordinator
  process is killed.
