---
title: Slice 031 p2panda-net Process Serving
status: completed
completed: 2026-05-18
plan: MVP/slice-031-p2panda-net-process-serving-plan.md
---

# Slice 031 p2panda-net Process Serving

Slice 031 moves the p2panda-net fact-node proof across an OS process boundary.
The serving/projection process owns a persistent p2panda store, a
`PandaNetFactNode`, an import/apply loop, SQLite projection rebuilds, snapshot
writes, and last-good gateway/DNS serving state. The local coordinator socket is
absent from the update path.

## What Shipped

- `p2panda-net-process-serving-contract` in `mvp-e2e -- all`.
- A `p2panda-net-serving-projection` process role that imports authorized
  p2panda-net fact operations into a local persistent store and automatically
  projects/reloads serving state after accepted facts.
- A scripted remote publisher role that keeps one p2panda-net peer alive long
  enough to publish baseline serving state, a delayed serving update, and a
  malformed marker.
- Structured p2panda-net status on serving roles: import/rejection counts,
  failure details, last reload, and bootstrap ticket.
- `PandaNetFactNode::refresh_stream`, used by the process receiver after idle
  timeouts so later appends from a stable remote peer are picked up.

## Proof

The contract proves:

- serving/projection imports baseline serving facts over p2panda-net,
- a local mutation attempt fails because no coordinator socket is running,
- a delayed remote serving update still imports, projects, and reloads,
- malformed network bodies are rejected without corrupting last-good serving,
- deleting `projections.sqlite` rebuilds from the receiver's local p2panda
  store while serving continues,
- restarting the serving/projection process without a coordinator reloads the
  last-good snapshots and local p2panda store.

Latest observed metrics:

```json
{
  "remote_updates_imported": 2,
  "malformed_messages_rejected": 1,
  "serving_process_alive_after_update": true,
  "elapsed_ms": 4436
}
```

## Semantic Leverage

This adds roughly 1.3k lines of harness plus contract code, but no deploy, ACME,
machine, gateway, or DNS business logic had to grow. The same serving facts,
projection reducer, snapshot reload, and p2panda authorization/import substrate
drive the proof. The useful product behavior is in the composition of primitives
rather than another bespoke coordinator path.

## Follow-Up

The current contract covers malformed network rejection at process level.
Wrong-island, untrusted-author, and unauthorized-replica rejection are already
covered in `p2panda-net-fact-node-contract`; a later hardening slice can mirror
those exact rejection classes through process-role status if we need process
coverage for every rejection variant.
