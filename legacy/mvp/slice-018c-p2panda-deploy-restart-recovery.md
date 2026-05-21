---
title: Slice 018c p2panda Deploy Restart Recovery
status: completed
completed: 2026-05-18
plan: MVP/slice-018c-p2panda-deploy-restart-recovery-plan.md
---

# Slice 018c p2panda Deploy Restart Recovery

## What Shipped

Slice 018c moves the deploy restart-recovery proof onto the p2panda-backed fact
boundary.

The shipped proof uses p2panda-backed facts for:

- the deploy decision written before participant mutation,
- the serving commit that is the route/gateway/DNS cutover boundary,
- the cleanup-done fact that makes recovery idempotent.

It also adds narrow p2panda operation export/import support so signed operation
exchange is proven separately from the deploy coordinator.

## Implementation Notes

`mvp-p2panda-facts` now exposes opaque `PandaFactOperation` values plus
`export_operations`/`import_operation`. Export is an iterator over stored
operations rather than a full-vector clone. Import validates the p2panda
operation, requires same-island ingestion, checks the original author's
fact-write grant, requires the claimed Ployz author to match a trusted p2panda
author key, and leaves read authorization to the normal `FactSource`
candidate/payload boundary. Payload reads match exact stored fact identity
before returning content by hash, so caller-supplied candidates cannot relabel
private content as a readable fact.

The deploy and serving p2panda writers remain E2E-local adapters. That keeps
deploy/routing semantics Ployz-owned and prevents `mvp-deploy` from depending
on p2panda details before the adapter shape has survived more than one command.

`deploy-restart-recovery-contract` kills the deploy coordinator object, exports
the surviving p2panda operations, imports them into a fresh fact store, and
recovers pending cleanup from that imported fact boundary. Persistent p2panda
storage and fact-store process restart remain future substrate work.

## Proof

Verified so far:

```text
cargo fmt --all
cargo clippy -p mvp-p2panda-facts -p mvp-deploy -p mvp-e2e --all-targets -- -D warnings
cargo test -p mvp-p2panda-facts --lib
cargo test -p mvp-deploy --lib
cargo run -p mvp-e2e -- p2panda-fact-source-contract
cargo run -p mvp-e2e -- deploy-commit-drain-contract
cargo run -p mvp-e2e -- deploy-restart-recovery-contract
MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
git diff --check
```

`deploy-restart-recovery-contract` asserts:

- no drain/stop before projection catch-up,
- projection catch-up happens after the coordinator object is dropped,
- the serving actor answers typed gateway/DNS queries while the coordinator is
  absent,
- recovery imports p2panda operations and reads deploy decision and serving
  commit facts instead of replaying pre-commit work,
- capacity/prepare/start are not re-run after restart,
- cleanup-pending after restart preserves visible nodes and serving commit id,
- final cleanup writes cleanup-done,
- later recovery returns complete without RPC.

Latest sample metrics:

```json
{
  "visible_nodes_at_decision": 3,
  "decision_fact_write_ms": 1,
  "serving_fact_write_ms": 0,
  "cleanup_done_fact_write_ms": 0,
  "projection_catch_up_ms": 9,
  "data_plane_requests_served_during_outage": 2,
  "capacity_requests": 3,
  "prepare_requests": 3,
  "start_requests": 3,
  "drain_requests": 2,
  "stop_requests": 1,
  "cleanup_pending_after_restart": true,
  "cleanup_done_recovered": true
}
```

The simplify/review pass also tightened the p2panda fact substrate:

- operation import authorizes the signed fact author, not the importing session,
  while still requiring same-island ingestion,
- operation import binds the claimed Ployz principal to a trusted p2panda
  author key,
- cross-principal import is covered by a unit test,
- operation bytes stay opaque to callers and export does not clone the full
  operation history by default,
- payload reads require exact stored fact identity before using the content-hash
  payload index,
- write/import duplicate and conflict classification share one helper,
- cleanup target assertions require the exact expected drain/stop sequence.

## Semantic Leverage

The deploy restart path still reads as a small product invariant:

```text
decision fact -> participant mutation -> serving commit
coordinator dies
read facts -> projection proof -> drain -> stop -> cleanup done
```

The maintenance-burden check is mixed but useful. Deploy is the strong win:
roughly 1.8k LOC in `MVP/deploy` versus about 9.8k old product-ish deploy LOC
when including the old deploy coordinator and related handlers. ACME and
gateway/DNS have not shown the same LOC reduction yet because shared primitives
and placeholder serving roles carry the current cost.

The next simplification targets are:

- one typed fact writer/key codec helper instead of repeated adapters,
- domain reducer composition instead of one growing projection reducer,
- moving reusable process-role pieces out of E2E once a production role needs
  them.

## Deferred

- Pre-serving/pre-commit candidate adoption and cleanup ABI.
- Real runtime/Docker/ZFS participant backends.
- p2panda persistent storage and fact-store process restart.
- p2panda network sync/discovery/blobs.
- Production WireGuard adapter proof.
- Pingora and `hickory-server` migration, if they remain the chosen serving
  primitives.
