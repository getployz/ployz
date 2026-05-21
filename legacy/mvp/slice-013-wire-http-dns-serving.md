---
title: Slice 013 Wire HTTP/DNS Serving While Coordinator Dies
status: completed
plan: MVP/slice-013-wire-http-dns-serving-plan.md
created: 2026-05-18
---

# Slice 013 Wire HTTP/DNS Serving While Coordinator Dies

## Result

This slice moves the coordinator-down serving proof from typed IPC queries to
real wire traffic inside `MVP/`.

- `mvp-serving` now has an HTTP gateway role that routes by `Host` from
  last-good serving state and proxies through the selected loopback backend.
- `mvp-serving` now has a DNS role that parses real UDP DNS queries with
  Hickory protocol types and answers projected AAAA records from last-good
  serving state.
- `mvp-e2e role http-gateway` and `mvp-e2e role dns-server` run as separate OS
  process roles with Unix control sockets for readiness, reload, status, and
  shutdown.
- `wire-serving-contract` kills the local coordinator, proves HTTP/DNS keep
  answering real socket traffic, injects later already-authorized serving facts,
  projects/reloads them while the coordinator stays dead, and restarts HTTP/DNS
  roles from snapshots.
- Wire request hot paths read from a shared last-good snapshot holder. The
  Kameo serving actor still owns reload/status state, but HTTP/DNS requests no
  longer serialize through its mailbox.

## Crate Decisions

Checked before and during implementation:

- `hyper`, `hyper-util`, and `http-body-util` were used instead of Pingora for
  this proof. The fallback still proxies to a deterministic backend and asserts
  the backend response; Pingora-specific lifecycle/proxy integration remains
  unproven.
- `hickory-proto = 0.26.1` was used instead of `hickory-server`. The role still
  parses and encodes real DNS messages on the patched Hickory protocol line;
  `hickory-server` handler integration and TCP fallback remain unproven.
- `arc-swap` was deferred. A simple shared `RwLock` snapshot holder removes the
  actor-mailbox bottleneck while keeping the code easy to inspect. Lock-free
  immutable snapshots can be revisited after traffic measurements justify them.
- `notify`, TLS, DNSSEC, DoH/DoT/DoQ, recursive DNS, ACME challenge serving,
  health checking, and production supervision remain deferred.

## Proof

Checks run:

```text
cd MVP && cargo fmt --all
cd MVP && cargo test -p mvp-serving
cd MVP && cargo test -p mvp-e2e
cd MVP && cargo clippy -p mvp-serving --tests -- -D warnings
cd MVP && cargo clippy -p mvp-e2e --tests -- -D warnings
cd MVP && cargo run -p mvp-e2e -- process-role-serving-contract
cd MVP && cargo run -p mvp-e2e -- wire-serving-contract
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

Review/simplify fixes landed separately:

- Removed the unused exported `WireRoleStatus` and typed wire-role listen
  addresses as `SocketAddr`.
- Shared process-role E2E child spawning, remote injection, readiness, rebuild,
  and cleanup helpers between process-role scenarios.
- Added bounded HTTP connection tasks and DNS packet tasks, with shutdown
  cancellation.
- Made dropped HTTP/DNS handles send shutdown and abort detached tasks.
- Hardened process-role PID cleanup with executable identity, best-effort
  sweep semantics, stale-record removal, and pid-file retention when a running
  child is dropped.
- Moved HTTP/DNS lookup hot paths off the Kameo actor mailbox and added
  concurrent wire-serving tests.
- Expanded the wire E2E to cover corrupt, missing, and wrong-island snapshot
  reload failures, fresh rebuilt snapshots, required metric assertions, and
  malformed DNS metrics without timing sleeps.

Observed `wire-serving-contract` metrics from the final `all` run:

```text
coordinator_killed: true
http_outage_success_count: 3
dns_outage_success_count: 3
rebuild_probe_success_count: 2
local_mutation_failure_after_death: connect local coordinator socket ... Connection refused
http_reload_latency_us: 216
dns_reload_latency_us: 242
projection_rebuild_us: 7621
http_restart_us: 12307
dns_restart_us: 11799
stale_snapshot_age_us: 23469
http_request_count: 1
dns_request_count: 3
malformed_dns_count: 1
backend_failure_count: 0
http_latency_samples_us: [148]
dns_latency_samples_us: [21, 0, 11]
elapsed_ms: 118
```

The final `all` run also completed the existing 10,000 logical-node bus,
bridge, projection, queue-group, and saturation checks inside the 120s budget.
The 10,000-node publish check delivered exactly 1,000,000 messages, the
10,000-node request-many check returned exactly 1,000,000 replies, and the
10,000-node projection rebuild projected 10,000 nodes plus 10,000 services with
the deadline satisfied.

## Semantic-Leverage Check

New wire serving code:

```text
MVP/serving/src/http_gateway.rs: 324 LOC
MVP/serving/src/dns_server.rs: 199 LOC
MVP/serving/src/wire.rs: 112 LOC
MVP/e2e/src/wire_serving_contract.rs: 663 LOC
```

The E2E harness is intentionally heavier than the serving logic because it
proves OS process fate separation, coordinator death, remote fact injection,
snapshot corruption, projection rebuild, wire reload, and restart. The product
semantics stay compact:

```text
ServingCommit fact
  -> projection writes gateway.snapshot/dns.snapshot
  -> explicit wire-role reload
  -> HTTP/DNS answer from last-good memory
```

The useful leverage is that adding real wire traffic did not require porting
the old daemon deploy/gateway control shape. HTTP/DNS serving is fed by the new
snapshot primitive, not by a bespoke event-log or daemon-owned store path.

## Covered And Deferred

Covered:

- Real HTTP request serving through a selected backend path.
- Real UDP DNS AAAA responses from projected DNS snapshots.
- Coordinator death before outage probes.
- Local mutation failure through the killed coordinator path.
- Remote serving fact injection while coordinator remains dead.
- Explicit projection/reload into HTTP/DNS wire roles.
- Corrupt, missing, and wrong-island next snapshots preserving last good state.
- Projection DB deletion and rebuild while wire roles continue serving.
- Fresh rebuilt snapshots changing HTTP/DNS answers after reload.
- HTTP/DNS role restart from snapshot files while coordinator remains dead.
- Wire metrics for outage success, reload/restart latency, request counts,
  malformed DNS, backend failure count, latency samples, and stale snapshot age.

Deferred:

- Pingora-specific gateway integration.
- `hickory-server` authoritative server integration and DNS TCP fallback.
- WireGuard service-to-service traffic while coordinator is down.
- Deploy coordinator crash/restart around drain.
- Docs-backed cross-node replication driving the same serving updates.
- Production role supervision, file watching, TLS, ACME, DNSSEC, and health
  checking.
