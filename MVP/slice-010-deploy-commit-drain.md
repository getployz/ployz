---
title: Slice 010 Deploy Commit Before Drain
status: active
plan: MVP/slice-010-deploy-commit-drain-plan.md
created: 2026-05-17
---

# Slice 010 Deploy Commit Before Drain

## Result

This slice implements the first deploy canary on the MVP primitives.

Implemented shape:

- `mvp-deploy` defines typed deploy IDs, phase IDs, instance IDs, node IDs,
  visible-node evidence, serving commits, phase state, deploy outcomes,
  participant wire payloads, and a bus-facing deploy coordinator.
- `ServingCommit` is one aggregate local durable route/gateway/DNS fact
  boundary. It is the drain gate. There is no quorum, `min_replicas`, witness
  ack, or `store.pin_fact` path.
- The coordinator can stop at serving commit so E2E can prove local projection
  catches up before old-instance stop.
- There is no one-shot public deploy API in this slice. Callers must explicitly
  run serving commit, observe/project the committed state, turn the projection
  report into `ProjectionCatchUp`, then finish cleanup. The proof validates
  the projected gateway/DNS IDs, revisions, routes, backends, old backends, and
  DNS records.
- Phase policy separates reversibility from serving publication, so a future
  serving phase can also be irreversible without encoding another ad hoc phase
  action.
- Capacity admission uses exact planned-node request-many probes. Open wildcard
  capacity remains useful as observation, but admission does not trust a node id
  merely because some wildcard responder put it in a payload. Planned-node
  admission also validates the capacity fields it requests.
- Old backends are removed from active `backends` and retained in
  `old_backends_to_drain`; old instances stay alive through projection catch-up
  and drain grace.
- Cleanup failure after serving commit returns `CleanupPending`, not deploy
  failure.
- Serving/gateway/DNS commit heads now reduce deterministically by epoch and
  content hash and surface `Superseded` status for non-winning candidates, but
  deploy uses the aggregate serving fact so cutover is one local write
  boundary.
- `mvp-e2e` includes `deploy-commit-drain-contract` in the scenario table.

## Crate Decisions

Checked before implementation:

- `statig`, `rust-fsm`, and `sm` were deferred. The slice state machine is
  small enough that explicit enums plus transition methods are easier to read.
- `petgraph` was deferred. The canary manifest is two explicit phases, not a
  dependency DAG.
- Existing MVP primitives carried the work: `mvp-bus` for queue/request/fact
  writes, `mvp-projection` for serving-state facts/snapshots, `serde_json` for
  harness payloads, and `thiserror` for structured deploy errors.

## Proof

Targeted checks run so far:

```text
cd MVP && cargo check -p mvp-deploy -p mvp-projection -p mvp-bus
cd MVP && cargo test -p mvp-deploy
cd MVP && cargo test -p mvp-projection
cd MVP && cargo test -p mvp-bus
cd MVP && cargo check -p mvp-e2e -p mvp-deploy
cd MVP && cargo run -p mvp-e2e -- deploy-commit-drain-contract
cd MVP && cargo test --all
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
cd MVP && cargo clippy --all-targets -- -D warnings
just test
```

Observed `deploy-commit-drain-contract` metrics:

```text
scheduler_queue_deliveries: 1
visible_nodes_at_decision: 3
phase_1_ms: 1
phase_2_ms: 0
local_route_commit_ms: 1
route_commit_to_projection_ms: 2
drain_requests: 1
stop_requests: 1
old_backend_alive_during_projection: true
cleanup_pending_count: 1
superseded_count: 1
elapsed_ms: 208
```

## Semantic-Leverage Check

Old deploy reference baseline:

```text
crates/ployzd/src/daemon/handlers/deploy.rs: 4558 LOC
crates/ployz-orchestrator/src/deploy/*.rs: 16599 LOC
old deploy sample total: 21157 LOC
```

New MVP deploy canary:

```text
MVP/deploy/src/*.rs: 1476 LOC
MVP/e2e/src/deploy_commit_drain_contract.rs: 985 LOC
MVP/e2e/src/projection_harness.rs: 23 LOC
```

This is a real semantic-leverage improvement for the canary's covered surface:
deploy business logic says "inspect capacity, start phases, write serving
commit, project, drain" instead of owning transport, serving-state storage,
projection, and cleanup semantics itself.

Parity matrix:

| Old deploy semantic | Slice 010 status |
| --- | --- |
| Submit deploy through scheduler path | Covered through `deploy.submit` queue proof |
| Capacity fanout/admission | Covered through exact planned-node `request_many` probes |
| Capacity fields affect admission | Covered through DB-capability rejection before mutation |
| Phase readiness before cutover | Covered by domain state machine and E2E |
| Irreversible phase failure | Covered as `DeployBlockedAfterIrreversiblePhase` |
| Route cutover before drain | Covered by `ServingCommit` |
| Gateway/DNS projection from serving truth | Covered through projection actor/snapshots and content-matched `ProjectionCatchUp` |
| Old backend kept alive during projection lag | Covered before old stop |
| Cleanup failure after cutover | Covered as `CleanupPending` |
| Concurrent serving-head race | Covered with deterministic `Superseded` |
| Real runtime/Docker/ZFS work | Deferred |
| Full daemon crash/restart recovery | Deferred to E2E-7 |
| Real HTTP/DNS serving process behavior | Deferred |

## Remaining Work

- Add real runtime/Docker/ZFS participants behind the typed participant
  commands.
- Prove real gateway and DNS serving roles consume the projected snapshots while
  the coordinator is down.
- Move deploy facts to real iroh-docs replication instead of the in-memory fact
  harness.
- Add full E2E-7 crash/restart recovery: destroy coordinator memory after
  serving commit, rebuild projection from facts, and resume or explicitly
  repair cleanup.
- Decide later whether deploy ownership needs advisory leases; this slice does
  not introduce them by default.
