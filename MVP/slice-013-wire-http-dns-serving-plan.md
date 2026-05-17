---
title: Slice 013 Wire HTTP/DNS Serving While Coordinator Dies Plan
status: completed
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-011-steady-state-serving.md
  - MVP/slice-012-process-role-serving.md
---

# Slice 013 Wire HTTP/DNS Serving While Coordinator Dies Plan

## Summary

Move the coordinator-down serving proof from typed harness queries to real wire
traffic under `MVP/`: an HTTP gateway process and a DNS process load last-good
snapshot state, answer real TCP/UDP requests, keep serving after the local
coordinator dies, reload later already-authorized serving facts through the
projection/snapshot path, and restart from snapshot files while mutation
authority is still absent.

This slice should make the HTTP/DNS data-plane contract tangible without
preserving the old gateway/DNS input model. Pingora and Hickory are serving
implementation candidates because they are good protocol primitives, not
because the old process shape is a constraint.

---

## Problem Frame

Slice 011 proved actor-owned last-good gateway/DNS state. Slice 012 proved real
OS process fate separation: killing the local mutation coordinator does not kill
the serving/projection process. The remaining gap in this part of E2E-7 is wire
behavior. Typed IPC queries are too kind to the design; they do not prove that
real HTTP clients and DNS clients keep working while the coordinator is gone.

The next proof should introduce MVP-local wire-serving roles:

- an HTTP gateway role that routes by host from last-good gateway snapshot
  state,
- a DNS role that answers authoritative records from last-good DNS snapshot
  state,
- a projection/applier role that can publish new snapshot files from durable
  facts without reviving the local coordinator,
- a harness that kills the coordinator and probes real HTTP/DNS sockets before
  any recovery command runs.

The important invariant remains unchanged:

```text
ServingCommit facts
  -> deterministic projection
  -> atomic gateway.snapshot / dns.snapshot
  -> explicit wire-role reload
  -> HTTP/DNS answers from actor-owned last-good memory
```

Wire handlers must not read SQLite, durable facts, bus state, or coordinator
state directly.

---

## Requirements

- R1. Add real wire HTTP and DNS serving roles under `MVP/`.
- R2. HTTP serving loads last-good `gateway.snapshot`, selects the route by
  `Host`, and sends a real HTTP response through the selected backend path.
- R3. DNS serving loads last-good `dns.snapshot` and answers a real UDP DNS
  query for an AAAA record from the projected snapshot.
- R4. HTTP and DNS wire roles answer from actor-owned last-good state, not from
  SQLite, facts, the bus, or the coordinator hot path.
- R5. A local coordinator process can write baseline serving facts, then be
  killed. After that kill, real HTTP and DNS requests still succeed before any
  recovery command runs.
- R6. A later serving commit injected as already-authorized remote replication
  can be projected and explicitly reloaded into HTTP/DNS wire roles while the
  local coordinator remains dead.
- R7. Corrupt, wrong-island, missing, or otherwise unsafe next snapshots do not
  replace last-good wire-serving state. HTTP/DNS continue answering the last
  good revision and status records a structured reload failure.
- R8. Deleting `projections.sqlite` while wire roles are live does not interrupt
  HTTP/DNS answers. A projection rebuild from durable facts can publish fresh
  snapshots, then wire roles can reload them explicitly.
- R9. HTTP and DNS wire roles can restart while the coordinator is still dead,
  load snapshots before any coordinator contact, and answer real wire requests.
- R10. The E2E report includes wire-specific metrics: HTTP success during
  coordinator outage, DNS success during coordinator outage, reload latency,
  restart latency, query latency percentiles or samples, and stale snapshot age.
- R11. All new code remains self-contained under `MVP/`.

---

## Scope Boundaries

- Keep all implementation under `MVP/`.
- Do not modify existing `crates/`, root workspace membership, existing
  gateway/DNS binaries, or existing daemon code.
- Do not preserve the old gateway/DNS input model as a constraint.
- Do not turn this into a production process supervisor. The process harness is
  proof substrate.
- Do not add automatic file watching in this slice. Projection and serving
  reloads remain explicit so tests can prove last-good behavior deterministically.
- Do not add TLS, ACME challenge serving, DNSSEC, DoH, DoT, DoQ, recursive
  resolution, dynamic DNS, or health checking in this slice.
- Do not add a member list, active-partition view, quorum, witness
  acknowledgements, `store.pin_fact`, or `min_replicas`. The operator's
  connected node remains the command consistency boundary. A future active
  member view may improve decision-time reachability evidence later, but it must
  not become a hidden peer-ack commit gate.
- Do not claim full E2E-7 completion. WireGuard service-to-service traffic,
  deploy coordinator crash/restart around drain, and docs-backed cross-node
  replication remain follow-up proof targets.

---

## Context & Research

### Relevant MVP Patterns

- `MVP/serving/src/actor.rs`: actor-owned last-good serving state, explicit
  reload, typed route/DNS lookup, and structured freshness/failure status.
- `MVP/serving/src/model.rs`: snapshot batch loading and lookup indexes for
  gateway routes and DNS records.
- `MVP/projection/src/snapshot.rs`: atomic gateway/DNS snapshot publication,
  island/schema validation, and symlink rejection.
- `MVP/e2e/src/process_role_harness.rs`: current process-role dispatch,
  harness-local Unix IPC, local coordinator role, remote replication injector,
  and serving/projection role.
- `MVP/e2e/src/process_role_serving_contract.rs`: current Slice 012 flow to
  extend or mirror with real wire probes.
- `MVP/deploy/src/serving_commit.rs`: aggregate serving commit shape that feeds
  route/gateway/DNS projection.

### Institutional Learnings

- `VISION.md`: the data plane outlives the control plane.
- `MVP/overall-plan.md`: killing the daemon means killing the role that accepts
  operator commands and coordinates mutations, not HTTP/DNS serving or local
  appliers.
- `MVP/architecture.md`: HTTP/DNS serving must load last-good local state before
  trying live control-plane connectivity.
- `MVP/e2e-proof-plan.md`: Slice 012 is still not the full E2E-7 wire proof;
  real HTTP/DNS serving is explicitly listed as follow-up work.
- `MVP/primitive-decisions.md`: the current typed serving proof is not real
  Pingora or Hickory wire serving, and `arc-swap` should be reconsidered only
  when wire handlers need hot-path concurrent reads.
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`:
  status must separate durable truth, live observation, and stale/unknown
  health.
- `docs/solutions/performance-issues/machine-add-timeout-tests-2026-05-10.md`:
  use bounded wait policies and deterministic readiness instead of sleeps.

### Crate Scout

Use now if the implementation stays small:

- `pingora = { version = "0.8.0", default-features = false, features = ["proxy"] }`
  for the HTTP gateway role. Pingora is the right family when the slice proves
  actual proxy/gateway behavior instead of a toy HTTP endpoint. Keep TLS
  features off for this first proof.
- `hickory-server = "0.26.1"` for authoritative DNS serving if a snapshot-backed
  handler can be implemented with a small surface.
- Add a direct `hickory-proto = "0.26.1"` dependency if needed to force the
  patched protocol line and to build DNS test queries/responses.
- Existing `tokio::process`, `tokio::net`, `serde_json`, and scenario artifact
  helpers for process-role orchestration.

Allowed fallbacks:

- `hyper = { version = "1.9", features = ["http1", "server"] }` plus
  `hyper-util` and `http-body-util` may be used only if Pingora lifecycle
  control makes the slice too large for the proof. If this fallback is used, it
  must still proxy to the selected deterministic loopback backend and assert the
  backend response. Only Pingora-specific proxying remains unproven.
- `hickory-proto = "0.26.1"` plus a minimal Tokio UDP authoritative responder
  may be used if `hickory-server` abstractions would dominate the slice. This is
  a fallback from the `hickory-server` abstraction, not a fallback away from the
  Hickory protocol family. The responder should still use Hickory message
  parsing/encoding rather than ad hoc DNS byte manipulation.

Defer:

- `arc-swap`: likely useful once wire request volume proves lock or actor
  mailbox contention. Keep the first implementation simple and measured.
- `notify`: file watching would obscure explicit reload semantics.
- `clap`: the role argument surface is still harness-only and narrow.
- DNSSEC, resolver, recursor, SQLite, DoH, DoT, DoQ, and TLS feature sets.

Security/version notes:

- Use Hickory `0.26.1`, not the old `0.25` line. RustSec reports a Hickory
  protocol message-encoding CPU exhaustion advisory patched in
  `hickory-proto >=0.26.1`, and a DNSSEC NSEC3 validation advisory that should
  be avoided by keeping DNSSEC features off.
- Use Pingora `0.8.0` or newer inside `MVP/`. RustSec reports a Pingora
  request-smuggling advisory for older `pingora-core` versions; `0.8.0` is the
  current patched line identified by the crate scout.

Sources:

- Pingora docs: <https://docs.rs/pingora/latest/pingora/>
- Pingora proxy docs: <https://docs.rs/pingora-proxy/latest/pingora_proxy/>
- RustSec Hickory package advisories:
  <https://rustsec.org/packages/hickory-proto.html>
- RUSTSEC-2026-0119 patched version:
  <https://rustsec.org/advisories/RUSTSEC-2026-0119.html>
- RUSTSEC-2026-0118 DNSSEC caveat:
  <https://rustsec.org/advisories/RUSTSEC-2026-0118.html>
- Hickory server docs:
  <https://docs.rs/hickory-server/latest/hickory_server/>
- Hyper server guide: <https://hyper.rs/guides/1/server/hello-world/>

---

## Key Technical Decisions

- Add wire serving as an adapter over `mvp-serving`, not as a replacement for
  the serving actor. The actor remains the state owner for loaded revisions,
  reload status, freshness, and last-good answers.
- Prefer separate process roles for HTTP gateway and DNS serving instead of
  folding wire listeners into the projection role. This is closer to the final
  data-plane shape and makes coordinator/projection/HTTP/DNS fate boundaries
  visible in tests.
- Keep projection and wire serving separate. Projection writes atomic snapshot
  files; wire roles reload snapshots into memory. Wire request paths do not
  project, reduce facts, or touch SQLite.
- Use real backend responses for HTTP in every implementation path. A
  deterministic E2E backend runs on loopback and returns its identity; the
  gateway snapshot points at that backend address. The proof must assert the
  client received the backend response, not just a mocked gateway string.
- Define a narrow MVP HTTP backend contract before proxy implementation:
  projected backend `address` is interpreted as a loopback `SocketAddr` with
  cleartext HTTP/1 for this slice. Empty, malformed, non-loopback, or
  unreachable backend targets return a structured `503` response and increment
  wire-role backend failure metrics. Wider URI/scheme support is a later
  serving-state schema decision.
- Keep DNS authoritative and narrow. The first DNS proof needs AAAA success,
  no-answer/unknown-host behavior, malformed packet handling, and case-insensitive
  lookup. Recursive resolution and DNSSEC do not belong in this slice.
- Keep reload explicit. The test harness sends a reload command after snapshot
  publication so stale-state behavior is deterministic and auditable.
- Failed reload tests must not permanently destroy the only restartable snapshot
  files. Either exercise reload failure through separate candidate paths, or
  explicitly republish/restore valid snapshots before restart. The E2E must
  prove both parts: failed reload preserves live in-memory answers, and restart
  succeeds only after valid snapshots are present again.
- Use dynamic loopback binds for HTTP, DNS, and deterministic test backends.
  Role readiness means: control socket responsive, wire socket bound, initial
  snapshot batch loaded, and resolved `SocketAddr` reported in role status.
- Keep the future member-list idea deferred. It may later enrich
  visible-node-at-decision-time evidence, but this slice must not smuggle it in
  as a commit gate or liveness-derived durable truth.

---

## Output Structure

Expected implementation shape:

```text
MVP/serving/
  Cargo.toml
  src/lib.rs
  src/http_gateway.rs
  src/dns_server.rs
  src/wire.rs
  src/actor.rs
  src/model.rs
  src/tests.rs

MVP/e2e/src/
  wire_serving_contract.rs
  process_role_harness.rs
  process_role_serving_contract.rs
  main.rs
```

This tree is guidance, not a hard constraint. Keep any adjusted shape small,
typed, and fully under `MVP/`.

---

## High-Level Technical Design

### Wire State Boundary

Introduce a small serving-side abstraction so wire handlers do not know about
snapshot files or projection:

```text
WireServingState
  gateway_route_for_host(host) -> Option<GatewayRouteProjection>
  dns_records(name, record_type) -> Vec<DnsRecordProjection>
  reload() -> ServingStatus
  status() -> ServingStatus
```

The initial implementation can delegate to `ServingActorHandle`. If Pingora or
Hickory integration shows that actor asks are too expensive under load, add a
measured follow-up to move hot-path reads to an immutable shared snapshot. Do
not add `arc-swap` speculatively in this slice.

Wire request health lives beside, not inside, `ServingStatus`:

```text
WireRoleStatus
  serving: ServingStatus
  listen_addr: SocketAddr
  request_count
  malformed_dns_count
  backend_failure_count
  latency_samples_or_percentiles
```

`ServingStatus` remains about snapshot loading and freshness. Protocol parse
errors, backend failures, and wire latency belong to the wire-role status and
scenario metrics.

### HTTP Gateway Role

The HTTP gateway role:

- starts with `--root`, `--control-socket`, `--listen`, `--island`,
- loads `gateway.snapshot` and `dns.snapshot` through `mvp-serving`,
- exposes a tiny role-control API for `status`, `readiness`, `reload`, and
  `shutdown`,
- listens on the provided TCP address,
- routes by `Host` header against the loaded gateway routes,
- proxies to the selected backend when a route exists,
- returns structured 404/503-style responses when no route or backend is
  available,
- keeps last-good state after failed reloads.

The E2E backend should be intentionally boring: a loopback HTTP server that
returns its backend id. The gateway test should assert that real client traffic
reaches the selected backend through the gateway.

### DNS Role

The DNS role:

- starts with `--root`, `--control-socket`, `--listen`, `--island`,
- loads the same validated snapshot batch through `mvp-serving`,
- exposes `status`, `readiness`, `reload`, and `shutdown`,
- answers UDP DNS queries for supported records from the last-good DNS
  snapshot,
- returns no-answer or NXDOMAIN-style responses for unsupported names/types
  according to the chosen Hickory API,
- handles malformed packets as structured role status/metrics without
  replacing last-good state.

If `hickory-server` is used, keep the handler authoritative and snapshot-backed.
If `hickory-proto` is used directly, use it for parsing and encoding and keep
manual server logic small.

### E2E Scenario

Add `wire-serving-contract` rather than overloading the Slice 012 scenario.
The new scenario should:

1. Start deterministic loopback backend `backend-1`.
2. Start serving/projection role.
3. Start local coordinator role.
4. Commit `serving-1` pointing gateway route at `backend-1` and DNS at
   `fd00::1`.
5. Project snapshots.
6. Start HTTP gateway and DNS wire roles from snapshot files.
7. Assert real HTTP and DNS wire answers for `serving-1`.
8. Kill the local coordinator.
9. Before any recovery command, assert real HTTP and DNS wire answers still
   succeed for `serving-1`.
10. Assert local mutation through the dead coordinator fails visibly.
11. Start backend `backend-2`.
12. Inject `serving-2` as already-authorized remote replication.
13. Project snapshots and explicitly reload HTTP/DNS wire roles.
14. Assert real HTTP and DNS answers switch to `serving-2`.
15. Corrupt, wrong-island, missing, or otherwise unsafe next snapshots, attempt
    reload, assert last-good wire answers remain `serving-2` and status records
    failure.
16. Restore or republish valid snapshots, then assert restartable snapshot files
    are valid again.
17. Delete `projections.sqlite`, rebuild projection while wire roles continue
    answering `serving-2` before explicit reload, then reload and assert answers
    remain correct.
18. Restart HTTP gateway while the coordinator is still dead; assert it loads
    snapshots and serves.
19. Restart DNS while the coordinator is still dead; assert it loads snapshots
    and serves.
20. Shutdown all children and toy backends with bounded cleanup.

---

## Implementation Units

### Unit 1: Wire State Adapter

Files:

- `MVP/serving/src/wire.rs`
- `MVP/serving/src/lib.rs`
- `MVP/serving/src/tests.rs`

Work:

- Add the smallest typed adapter layer over `ServingActorHandle` for wire
  handlers.
- Preserve structured `ServingError` and `ServingStatus` mapping.
- Keep status/reload behavior shared with typed serving paths.

Tests:

- Adapter returns route and DNS records from a loaded snapshot batch.
- Adapter reload failure preserves last-good answers and exposes the existing
  structured failure.
- Adapter status includes loaded revisions and snapshot age.

### Unit 2: HTTP Gateway Wire Role

Files:

- `MVP/serving/src/http_gateway.rs`
- `MVP/serving/Cargo.toml`
- `MVP/e2e/src/process_role_harness.rs`

Work:

- Add an HTTP gateway implementation over the wire state adapter.
- Prefer Pingora `0.8.0` if lifecycle and shutdown remain contained.
- Keep TLS off.
- Add a harness role command surface for `status`, `reload`, `shutdown`, and
  readiness.

Tests:

- Host match routes to the expected backend and returns the backend response.
- Unknown host returns a structured no-route HTTP response.
- Empty, malformed, non-loopback, or unreachable backend target returns
  structured `503` and increments backend failure status.
- Reload switches route/backend after snapshot publication.
- Failed reload preserves prior backend routing.
- Role shuts down within the scenario deadline.

### Unit 3: DNS Wire Role

Files:

- `MVP/serving/src/dns_server.rs`
- `MVP/serving/Cargo.toml`
- `MVP/e2e/src/process_role_harness.rs`

Work:

- Add an authoritative DNS wire implementation over the wire state adapter.
- Prefer `hickory-server = "0.26.1"` if the handler stays small; otherwise use
  `hickory-proto = "0.26.1"` with manual Tokio UDP.
- Keep resolver, recursor, DNSSEC, SQLite, TLS, DoH, DoT, and DoQ features off.
- Add role-control commands for readiness, reload, status, and shutdown.

Tests:

- AAAA query for the known name returns the expected IPv6 address.
- Case-insensitive name lookup works.
- Unknown name or unsupported type returns a deterministic no-answer result.
- Malformed packet does not crash the role.
- Reload switches DNS answer after snapshot publication.
- Failed reload preserves prior DNS answer.

### Unit 4: Wire E2E Contract

Files:

- `MVP/e2e/src/wire_serving_contract.rs`
- `MVP/e2e/src/process_role_harness.rs`
- `MVP/e2e/src/main.rs`
- `MVP/e2e/Cargo.toml`

Work:

- Add `wire-serving-contract` scenario with real HTTP and DNS probes.
- Use bounded TCP/UDP timeouts for every probe.
- Start deterministic loopback HTTP backends for gateway proxy proof.
- Register all child PIDs for global `all` timeout cleanup.
- Emit metrics under `MVP/target/mvp-e2e/wire-serving-contract/`.
- Validate the metrics schema before the scenario passes.

Tests:

- Scenario passes standalone.
- `mvp-e2e -- all` remains inside the existing 120s budget.
- Metrics assert exact success counts during coordinator outage, not approximate
  success.
- Metrics include HTTP/DNS outage success counts, reload latency, restart
  latency, query latency samples or percentiles, stale snapshot age, malformed
  DNS count, and backend failure count.
- HTTP/DNS probes succeed after `projections.sqlite` deletion and during
  rebuild, before explicit reload.
- Child cleanup runs on normal shutdown and timeout.

### Unit 5: Documentation And Decision Ledger

Files:

- `MVP/primitive-decisions.md`
- `MVP/e2e-proof-plan.md`
- `MVP/slice-013-wire-http-dns-serving.md`

Work:

- Record which HTTP/DNS crates were actually used and why.
- Record any fallback away from Pingora or from `hickory-server` as a remaining
  proof gap. A fallback to `hickory-proto` is still a Hickory-family DNS proof,
  but not a full `hickory-server` proof.
- Update E2E-7 current proof status with wire-serving coverage and remaining
  gaps.
- Add the slice report with metrics, tests run, and semantic-leverage notes.

Tests:

- Documentation references the actual scenario name and metrics.
- Decision ledger notes any security/version caveat from the crate scout.

---

## System-Wide Impact

- Strengthens E2E-7 by proving data-plane-style HTTP/DNS traffic while the
  coordinator is dead.
- Keeps the old gateway/DNS code as reference material only; the new role shape
  is driven by last-good snapshots and actor-owned serving state.
- Forces the snapshot schema to serve real wire paths, which may reveal whether
  route/DNS projection data is missing fields before a full deploy rewrite.
- Adds dependency pressure inside `MVP/` only. Root workspace policy and
  existing crates remain untouched.
- Creates a sharper boundary for later WireGuard and workload proof: once real
  HTTP/DNS answers are independent of coordinator liveness, the next data-plane
  gap is service-to-service traffic across last-applied WireGuard config.

---

## Risks And Mitigations

- Risk: Pingora lifecycle/shutdown behavior makes the E2E role hard to keep
  bounded.
  Mitigation: keep the first Pingora integration no-TLS and minimal; if it
  still dominates the slice, use the documented Hyper fallback and record
  Pingora proxying as unproven.
- Risk: `hickory-server` API shape dominates implementation.
  Mitigation: use `hickory-proto 0.26.1` for parsing/encoding with a minimal
  Tokio UDP authoritative loop, then record full Hickory server integration as
  remaining work.
- Risk: HTTP/DNS wire roles accidentally read files or facts on every request.
  Mitigation: route all request handling through the wire state adapter backed
  by `ServingActorHandle` or a measured immutable snapshot.
- Risk: port allocation and child cleanup make `mvp-e2e -- all` flaky.
  Mitigation: dynamic loopback binds reported through readiness/status,
  bounded readiness probes, child PID cleanup, and exact success counters.
- Risk: gateway and DNS reload independently and diverge.
  Mitigation: both roles load the same validated snapshot batch and expose
  revisions in status; E2E asserts gateway/DNS revisions move together.
- Risk: the future member-list idea leaks in as hidden quorum.
  Mitigation: keep all reachability evidence explicit in command results and
  out of fact commit semantics.

---

## Review Focus

- Does any wire-serving request path depend on coordinator liveness, SQLite,
  facts, or projection hot reads?
- Are Pingora/Hickory dependencies pinned to safe current lines inside `MVP/`?
- Is fallback away from Pingora or `hickory-server` documented as a proof gap if
  it happens?
- Are process roles split cleanly enough that killing coordinator, projection,
  HTTP, and DNS have distinct failure audiences?
- Are every TCP/UDP/projection/reload/child wait path time-bounded?
- Does corrupt next state preserve last-good wire answers?
- Does the scenario prove real client-visible behavior rather than internal
  typed query success?
- Is the implementation still simple enough that future business logic can use
  the serving primitive without knowing transport/projection choreography?

---

## Completion Criteria

- `cargo fmt --all --check` passes from `MVP/`.
- `cargo test -p mvp-serving` passes.
- Any crate-local tests added for `mvp-e2e` pass.
- `cargo clippy -p mvp-serving --tests -- -D warnings` passes.
- `cargo clippy -p mvp-e2e --tests -- -D warnings` passes if dependencies make
  it practical within the slice budget.
- `cargo run -p mvp-e2e -- wire-serving-contract` passes.
- `MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all` passes.
- `wire-serving-contract` metrics include the named R10 fields and exact outage
  success counts.

## Delivery Checklist

- A simplify pass lands as a separate commit after implementation.
- A subagent review pass runs before the implementation is reported as complete.
- The branch is pushed to the existing PR.
