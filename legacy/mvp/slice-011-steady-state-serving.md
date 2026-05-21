---
title: Slice 011 Steady-State Serving While Coordinator Is Down
status: completed
plan: MVP/slice-011-steady-state-serving-plan.md
created: 2026-05-18
---

# Slice 011 Steady-State Serving While Coordinator Is Down

## Result

This slice adds the first MVP-local serving-state proof:

- `mvp-serving` owns typed last-good gateway/DNS state behind a Kameo actor.
- Serving loads a validated `gateway.snapshot` + `dns.snapshot` batch before
  answering queries.
- Gateway host lookup and DNS record lookup read from in-memory actor state,
  not SQLite, the bus, or the command coordinator.
- Reload validates the full gateway/DNS batch before replacing in-memory state.
  Corrupt, wrong-island, missing, or symlinked next snapshots keep last-good
  state and record structured failure status.
- Serving status exposes loaded revisions, loaded time, snapshot age,
  freshness, reload attempts, reload timestamps, and the last structured
  reload failure.
- `steady-state-serving-contract` proves a local coordinator can be absent
  while a separate harness fact writer publishes serving commits, projection
  rebuilds from facts, serving reloads snapshots, and gateway/DNS typed queries
  keep succeeding.

The public proof is intentionally typed gateway/DNS query behavior. It does not
claim real Pingora HTTP serving, real DNS serving, OS process restart, WireGuard,
or workload traffic yet.

## Crate Decisions

Checked before implementation:

- `notify` was deferred. File watching would obscure the last-good replacement
  contract; explicit reload is deterministic.
- `pingora`, `pingora-proxy`, `hickory-server`, and `axum` were deferred.
  They are serving-process or wire-protocol choices. This slice proves the
  state boundary those roles should consume.
- `arc-swap` was deferred. Actor-owned state is enough before real concurrent
  wire handlers exist.
- Existing MVP primitives carried the work: `mvp-projection` loads snapshots,
  `mvp-bus` supplies local facts/sessions for E2E, `mvp-deploy` writes the
  aggregate `ServingCommit`, and Kameo owns serving state.

## Proof

Checks run:

```text
cd MVP && cargo fmt --all --check
cd MVP && cargo test -p mvp-serving
cd MVP && cargo clippy -p mvp-serving --all-targets -- -D warnings
cd MVP && cargo test --all
cd MVP && cargo clippy --all-targets -- -D warnings
cd MVP && cargo test -p mvp-e2e
cd MVP && cargo run -p mvp-e2e -- steady-state-serving-contract
cd MVP && cargo clippy -p mvp-e2e --all-targets -- -D warnings
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

Observed `steady-state-serving-contract` metrics from the final `all` run:

```text
local_coordinator_available_for_mutation: false
initial_projection_us: 2327
remote_projection_us: 1982
projection_rebuild_us: 2049
reload_us: 78
restart_from_snapshot_us: 33
serving_query_probes_during_outage: 7
serving_reload_attempts: 4
stale_snapshot_age_us: 1189
updated_gateway_revision: gateway:serving-2-gateway:serving-2-route
updated_dns_revision: dns:serving-2-dns
corrupt_reload_preserved_last_good: true
wrong_island_reload_preserved_last_good: true
deleted_reload_preserved_last_good: true
symlink_reload_preserved_last_good: true
elapsed_ms: 28
```

The full `all` run also completed the existing 10,000 logical-node scale cases
inside the 120s budget.

## Semantic-Leverage Check

Old gateway/DNS/service reference baseline:

```text
crates/ployz-gateway/src/*.rs: 5418 LOC
crates/ployz-dns/src/*.rs: 2136 LOC
crates/ployzd/src/services/*.rs: 2026 LOC
old gateway/DNS/service sample total: 9547 LOC
```

New MVP serving-state proof:

```text
MVP/serving/src/*.rs: 907 LOC
MVP/e2e/src/steady_state_serving_contract.rs: 469 LOC
new serving proof total: 1376 LOC
```

This is not a complete gateway/DNS replacement. The useful leverage is that the
serving boundary now says: load a validated fact-derived snapshot batch, answer
typed queries from actor-owned last-good state, preserve last good on unsafe
reloads, and make freshness/failure visible. It does not import the old
NATS-store synchronization shape or preserve the old gateway input model.

## Covered And Deferred

Covered:

- Serving starts from local snapshot files without a coordinator.
- Serving keeps answering typed gateway/DNS queries while the coordinator is
  absent.
- A separate harness serving-commit writer projects into new snapshots; serving
  reloads those snapshots without local coordinator involvement.
- Deleting `projections.sqlite` does not interrupt serving queries.
- A fresh projection actor rebuilds from facts and republishes snapshots.
- Serving is queried while that rebuild task is in flight.
- Serving restarts from snapshot files while the coordinator remains absent.
- Unsafe reloads preserve last good and record structured status failure.

Deferred:

- Real process-role split: coordinator, projection/applier, gateway, and DNS as
  separate `MVP/` processes.
- Pingora HTTP serving and Hickory DNS serving.
- WireGuard service-to-service traffic while the coordinator is down.
- Docs-backed serving commits over real iroh replication in this scenario.
- Active-member or partition-view reachability policy. Future slices may add
  this as command evidence, but not as hidden quorum or a commit gate.
