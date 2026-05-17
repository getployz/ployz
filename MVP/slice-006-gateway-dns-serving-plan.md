---
title: Superseded Slice 006 Gateway DNS Serving Roles Plan
status: superseded
created: 2026-05-17
origin:
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/slice-005-fact-projection.md
---

# Slice 006 Gateway DNS Serving Roles Plan

## Superseded

This plan was superseded on 2026-05-17 before implementation was committed.
It incorrectly treated preserving the old gateway/DNS role shape as the next
proof target. The corrected strategy is to preserve HTTP/DNS product behavior
and data-plane continuity, while allowing the serving-state shape and process
boundary to be redesigned.

The next product proofs are ACME on the new primitives, followed by deploy
commit-before-drain. Do not execute this plan without first running a new
planning pass against `MVP/overall-plan.md`.

## Problem Frame

Slice 005 proved that facts reduce into SQLite plus `gateway.snapshot` and
`dns.snapshot`. The next missing proof is that serving roles can consume those
snapshots without depending on a live daemon or projection actor.

The MVP architecture requires gateway and DNS to be separate serving roles that
keep the last good in-memory snapshot while the daemon restarts. This slice
adds the MVP-local serving-role adapter for that behavior. It does not migrate
the existing `crates/ployz-gateway` or `crates/ployz-dns` binaries yet.

## Scope

Build an MVP-local serving role crate that:

- loads `gateway.snapshot` and `dns.snapshot` files written by
  `mvp-projection`,
- stores last-good gateway and DNS state in actor-owned memory,
- answers simple gateway route and DNS record queries from memory,
- reloads snapshots on explicit typed commands,
- rejects corrupt, wrong-island, missing, or symlinked next snapshots without
  replacing last good state,
- reports structured freshness/health so stale-state serving is visible,
- proves the serving-state adapter can restart from files while the projection
  actor is not running,
- proves gateway/DNS keep serving last good data after unsafe reload attempts.

Everything stays under `MVP/`.

This slice is the adapter proof before the process-boundary proof. It must not
claim the full E2E-7 daemon/gateway/DNS restart requirement is complete until a
later slice starts separate MVP-local `gateway` and `dns` processes while the
daemon/projection role is absent.

## Out Of Scope

- No changes outside `MVP/`.
- No migration into existing `crates/ployz-gateway` or `crates/ployz-dns`.
- No full Pingora server integration in this slice.
- No wire-level DNS server in this slice.
- No filesystem watcher dependency in this slice.
- No iroh, iroh-docs, or iroh-blobs integration in this slice.
- No process supervisor or sidecar spawning in this slice.

These are slice boundaries, not MVP reductions. The full MVP still needs the
separate-process gateway/DNS restart harness from `MVP/e2e-proof-plan.md`.

## Crate Scout

Plumbing this slice could otherwise invent:

- file reload notification,
- HTTP reverse-proxy serving,
- DNS wire serving,
- actor-owned snapshot state,
- structured local query APIs for tests and future roles.

Crates checked:

- `notify`: cross-platform filesystem notifications. It is a good later fit
  for automatic reloads, but this slice should use explicit reload commands so
  tests prove the semantic boundary before adding watcher timing behavior.
  Source: <https://docs.rs/notify/>
- `pingora-proxy`: the existing gateway migration target. Defer here because
  the next proof is snapshot adapter and last-good state, not proxy I/O.
  Source: <https://docs.rs/pingora-proxy>
- `hickory-server`: the right full DNS-server crate when the MVP needs
  wire-level DNS proof. Defer here because the next proof can be expressed with
  typed record queries against the same snapshot state.
  Source: <https://docs.rs/hickory-server/latest/hickory_server/index.html>
- `axum`: useful for future HTTP-process smoke tests, but unnecessary for this
  slice because gateway semantics can be proved with typed in-memory route
  queries and explicit actor commands.
  Source: <https://docs.rs/axum/latest/axum/index.html>

Decision: add no new runtime dependencies beyond `kameo`, `thiserror`,
`tokio`, and local MVP crates. Copy the existing gateway/DNS shared-snapshot
shape from the old crates conceptually: hot-path reads from in-memory
snapshots, reload path validates before replace.

## Existing Patterns To Follow

- `MVP/projection/src/snapshot.rs`: snapshot loaders already validate schema,
  island, and symlink targets.
- `MVP/projection/src/actor.rs`: actor-owned state, typed status, explicit
  command/reply messages.
- `crates/ployz-gateway/src/snapshot.rs`: serving hot path uses shared
  in-memory snapshot state and replaces it only from a validated projection.
- `crates/ployz-dns/src/snapshot.rs`: DNS serving reads from an in-memory
  snapshot.

## Implementation Units

### U1: Add `mvp-serving` crate

Files:

- Create `MVP/serving/Cargo.toml`
- Create `MVP/serving/src/lib.rs`
- Create `MVP/serving/src/error.rs`
- Modify `MVP/Cargo.toml`

Approach:

- Add a small library crate for MVP-local gateway/DNS serving state.
- Depend on `mvp-projection` for snapshot file structs/loaders.
- Use `thiserror` for structured serving errors.
- Keep public exports narrow.

Test scenarios:

- Crate compiles in the MVP workspace.
- Serving errors expose typed classes instead of string-only failures.

Verification:

- `cd MVP && cargo test -p mvp-serving`

### U2: Gateway Serving Actor

Files:

- Create `MVP/serving/src/gateway.rs`
- Modify `MVP/serving/src/lib.rs`

Approach:

- `GatewayServingActor` owns:
  - expected island,
  - snapshot path,
  - last-good `GatewaySnapshotFile`,
  - status containing loaded revision, route count, `loaded_at`,
    `last_success_at`, `last_reload_attempt_at`, last reload failure, source
    file modified time when available, snapshot age, and a freshness enum.
- `start` loads the snapshot file before serving.
- `reload` validates and replaces only on success.
- `route_for_host(host)` returns the current projected route from memory.
- If a reload fails, the previous route remains available and status records
  the failure.
- Missing snapshot files are unsafe reload failures only when the serving actor
  already has last-good state. They are not used to model a valid empty
  projection because `mvp-projection` intentionally removes snapshot files for
  healthy empty views. Valid empty serving state must be represented later by
  an explicit empty snapshot or another unambiguous projection marker.

Test scenarios:

- Startup fails if no valid gateway snapshot exists.
- Startup loads a valid snapshot and route lookup succeeds.
- Reload to a newer snapshot replaces last good.
- Corrupt, wrong-island, missing, or symlinked reload keeps last good and
  records a structured failure.
- Freshness distinguishes `Fresh`, `ServingAgedSnapshot`, and
  `ServingLastGoodAfterFailure`.

Verification:

- Gateway actor unit tests.

### U3: DNS Serving Actor

Files:

- Create `MVP/serving/src/dns.rs`
- Modify `MVP/serving/src/lib.rs`

Approach:

- `DnsServingActor` mirrors the gateway actor for `DnsSnapshotFile`.
- `record(name, record_type)` reads current records from memory.
- Reload semantics match gateway: validate before replace, preserve last good
  on failure, expose freshness/health.
- Missing snapshot files follow the same ambiguity rule as gateway: startup
  fails without a valid snapshot, while an existing actor treats missing reload
  as unsafe and preserves last good state.

Test scenarios:

- Startup fails if no valid DNS snapshot exists.
- Startup loads a valid snapshot and record lookup succeeds.
- Reload to a newer snapshot replaces last good.
- Corrupt, wrong-island, missing, or symlinked reload keeps last good and
  records a structured failure.
- Freshness distinguishes `Fresh`, `ServingAgedSnapshot`, and
  `ServingLastGoodAfterFailure`.

Verification:

- DNS actor unit tests.

### U4: Gateway/DNS Last-Good Serving Contract

Files:

- Create `MVP/e2e/src/serving_contract.rs`
- Modify `MVP/e2e/src/main.rs`
- Modify `MVP/e2e/Cargo.toml`
- Modify `MVP/README.md`

Approach:

- Reuse Slice 005 projection facts to write `gateway.snapshot` and
  `dns.snapshot`.
- Start gateway and DNS serving actors from only the snapshot files.
- Drop the projection actor/bus source to simulate daemon/projection outage.
- Query gateway and DNS actors to prove serving continues.
- Restart serving actors from the same snapshot files to prove the
  pre-process serving-state restart contract.
- Corrupt, wrong-island, symlink, or delete next snapshot files and prove reload
  failures preserve last good.
- Assert status exposes loaded revision, loaded-at time, last reload attempt,
  last success, failure kind, source age, and freshness. Projection outage and
  restart-from-file paths must be visible as last-good/aged serving rather than
  silently healthy.
- Emit structured metrics JSON under `MVP/target/mvp-e2e/serving-contract/`.

Test scenarios:

- `serving-contract` passes with gateway and DNS loaded from projection
  snapshots.
- Gateway/DNS continue serving after the projection actor is dropped.
- Gateway/DNS actor state restarts from snapshot files while projection is
  absent.
- Corrupt gateway/DNS reloads do not replace last good.
- Wrong-island gateway/DNS reloads do not replace last good.
- Symlinked gateway/DNS reloads do not replace last good.
- Deleted snapshot reloads do not replace last good.
- Status reports aged and last-good-after-failure state after outage/reload
  failure.

Verification:

- `cd MVP && cargo run -p mvp-e2e -- serving-contract`
- `cd MVP && cargo run -p mvp-e2e -- all`

### U5: Scale And Semantic-Leverage Evidence

Files:

- Modify `MVP/e2e/src/scale.rs`
- Modify `MVP/slice-006-gateway-dns-serving.md`
- Modify `MVP/primitive-decisions.md`

Approach:

- Extend scale output with a serving snapshot reload/read case for 200, 1,000,
  and 10,000 projected gateway backends.
- Measure actor startup/load duration, reload duration, gateway route lookup,
  and DNS record lookup.
- Record semantic leverage: the E2E should express "serving role keeps last
  good snapshot" in a few typed actor calls, not by scripting transport or store
  internals.

Test scenarios:

- Scale still passes existing bus/bridge/projection gates.
- Serving read/reload gate passes for 200, 1,000, and 10,000 projected gateway
  backends.
- Metrics include serving startup/reload/read durations.

Verification:

- `cd MVP && cargo run -p mvp-e2e -- scale`
- `cd MVP && just test`

## Review Risks

- Accidentally making missing snapshots look healthy.
- Treating projection's valid empty-view deletion as equivalent to an unsafe
  reload failure.
- Replacing last good state after corrupt or wrong-island files.
- Overfitting tests to in-process actor calls and losing the process-role
  migration path.
- Creating a second snapshot schema instead of reusing `mvp-projection`.
- Leaking projection actor or bus dependencies into the serving hot path.

## Semantic-Leverage Check

This slice proves a real product rule:

> Gateway and DNS keep serving the last good snapshot while the daemon or
> projection path is down.

The desired code shape is:

- projection writes snapshots,
- serving actor loads snapshots,
- typed queries read last-good memory,
- reload failure updates visible status without changing served data.

Future business code should not know about fact reducers, SQLite, bus
notifications, or projection actors to answer a serving query.

## Maintainer Documentation

Update `MVP/primitive-decisions.md` with:

- why serving roles read snapshot files first,
- why reload is explicit in this slice instead of using `notify`,
- why Pingora and Hickory are deferred but remain the migration targets,
- the last-good state invariant and failure audience.

Create `MVP/slice-006-gateway-dns-serving.md` after implementation with proof
commands, metrics, review findings, and follow-ups.

Follow-up that must remain on the MVP proof map: add an MVP-local
separate-process smoke test that starts `gateway` and `dns` roles from snapshot
files while the daemon/projection role is absent. This slice only proves the
serving-state adapter those roles will use.
