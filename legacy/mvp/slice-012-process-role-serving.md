---
title: Slice 012 Process-Role Serving While Coordinator Dies
status: completed
plan: MVP/slice-012-process-role-serving-plan.md
created: 2026-05-18
---

# Slice 012 Process-Role Serving While Coordinator Dies

## Result

This slice moves the Slice 011 steady-state serving proof across real OS
process boundaries inside `MVP/`:

- `mvp-e2e role local-coordinator` owns the harness-local mutation API and
  durably writes a serving commit.
- `mvp-e2e role serving-projection` owns projection plus serving actors behind a
  typed Unix-socket API.
- `mvp-e2e role remote-replication-injector` writes one already-authorized
  replicated fact without reviving local mutation authority.
- `process-role-serving-contract` kills the local coordinator after the first
  serving state is projected and loaded, then proves the serving/projection
  process keeps answering gateway/DNS queries before any recovery command runs.
- Later remote-injected facts still project and reload in the already-running
  serving/projection process.
- Deleting `projections.sqlite` does not interrupt serving queries; a fresh
  projection actor rebuilds from file-backed facts while last-good serving
  remains queryable.
- The serving/projection process can restart from snapshot files while the
  coordinator remains dead.

The proof is intentionally still typed gateway/DNS query behavior, not real
Pingora HTTP serving or Hickory DNS serving. It also does not claim real
iroh-docs cross-node replication; `process_fact_source` is a harness-only
file-backed fact source for OS fate separation.

## Crate Decisions

Checked before and during implementation:

- `tokio::process::Command` was adopted for child role lifecycle. Children use
  bounded waits and kill-on-drop where applicable.
- `tokio::net::UnixListener` / `UnixStream` remain the harness-local IPC
  primitive. `interprocess` stays deferred until cross-platform local sockets
  matter.
- `assert_cmd` was not added. Long-lived role tests need process handles, typed
  IPC, and explicit kill/wait behavior.
- `clap` was not added. The role command surface is harness-only and still
  small enough for explicit flag parsing.
- `tokio-util` cancellation and process supervisor crates were deferred. The
  slice needs bounded child cleanup, not a production supervisor.

## Proof

Checks run:

```text
cd MVP && cargo fmt --all --check
cd MVP && cargo test -p mvp-e2e process_role
cd MVP && cargo clippy -p mvp-e2e --tests -- -D warnings
cd MVP && cargo run -p mvp-e2e -- process-role-serving-contract
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

Review/simplify fixes landed separately after the implementation commit:

- Durable coordinator writes moved off the async IPC path through a reusable
  writer and `spawn_blocking`.
- Dropped coordinator clients no longer terminate the coordinator process.
- Remote injection returns typed success/failure instead of erasing validation
  and storage failures into strings.
- Role children register PID files so the `all` timeout path can best-effort
  kill process-role children if the scenario worker is abandoned.
- The outage probe metric now counts only the coordinator-death pre-recovery
  window; rebuild-time serving proof is recorded separately.

Observed `process-role-serving-contract` metrics from the final `all` run:

```text
coordinator_killed: true
serving_process_alive_after_kill: true
coordinator_outage_query_probes: 3
rebuild_query_probes: 1
local_mutation_failure_after_death: connect local coordinator socket ... Connection refused
commit_to_reload_us: 7059
remote_commit_to_reload_us: 12580
projection_rebuild_us: 10734
serving_restart_us: 28322
stale_snapshot_age_us: 483
baseline_gateway_revision: gateway:serving-1-gateway:serving-1-route
updated_gateway_revision: gateway:serving-2-gateway:serving-2-route
elapsed_ms: 115
```

The final `all` run also completed the existing 10,000 logical-node bus,
bridge, projection, queue-group, and saturation checks inside the 120s budget.

## Semantic-Leverage Check

New process-role proof code:

```text
MVP/e2e/src/process_fact_source.rs: 682 LOC
MVP/e2e/src/process_role_harness.rs: 1844 LOC
MVP/e2e/src/process_role_serving_contract.rs: 594 LOC
```

This is harness-heavy because it proves process fate separation and cleanup.
The business invariant stays compact in the scenario flow: commit serving-1,
project/reload, kill coordinator, prove last-good queries, fail local mutation,
inject serving-2, project/reload, rebuild SQLite, restart serving from
snapshots.

The important semantic improvement is not raw LOC yet. It is that "daemon
down" is now represented as a killed mutation role plus healthy serving role,
instead of as a single process owning command routing, projection, and serving
state.

## Covered And Deferred

Covered:

- Real OS process split between local mutation authority and serving/projection.
- Coordinator kill after durable serving commit acknowledgement.
- Gateway/DNS typed queries continue after coordinator kill and before later
  projection/reload/rebuild commands.
- Local mutation attempts through the killed coordinator path fail visibly.
- Remote-replication injection is a separate role and author, not a revived
  local coordinator.
- Projection rebuild from file-backed facts while serving keeps answering.
- Serving/projection process restart from snapshot files while coordinator is
  absent.
- `mvp-e2e -- all` remains time-budgeted and now has a process-role child
  cleanup hook.

Deferred:

- Real Pingora HTTP request serving and real DNS serving.
- WireGuard and workload service-to-service traffic while the coordinator is
  down.
- Real iroh-docs remote replication in this process-role scenario.
- Deploy coordinator crash/restart around phase commit and drain.
- Active-member or partition-view reachability evidence. Future slices may add
  this as explicit command evidence; it is not a hidden quorum or commit gate.
