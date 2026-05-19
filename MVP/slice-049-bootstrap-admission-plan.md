---
title: Slice 049 Bootstrap Admission Plan
status: implemented
created: 2026-05-19
origin:
  - MVP/three-server-product-vertical-plan.md
  - MVP/slice-048-product-membership-foundation.md
---

# Slice 049 Bootstrap Admission Plan

## Goal

Close the first real membership convergence gap in the product binary: after a
node joins from an invite, the bootstrap node must durably learn the joiner's
p2panda ticket and fact-author key so both nodes can exchange signed membership
facts over p2panda-net.

## Scope

In scope:

- stable p2panda tickets that survive daemon restarts,
- issued invite records under the bootstrap node state,
- joined-node invite credentials persisted in local state,
- `mvp-node admission --state <joined-node>`,
- `mvp-node admit --state <bootstrap-node> --request <json>`,
- two-node membership convergence through product daemon runs and real
  p2panda-net fact transport.

Out of scope:

- automatic request/reply admission over an always-running daemon,
- three-node transitive authority propagation,
- membership authority facts backed by `mvp-p2panda-authz`,
- long-running daemon state hot reload.

## Design

The admission handoff is explicit and durable:

1. Bootstrap `invite` records the issued invite secret and expiry locally.
2. Joined node `join` stores the invite credentials it used.
3. Joined node `admission` emits its stable p2panda ticket, principal,
   fact-author key, WireGuard identity, and invite proof.
4. Bootstrap `admit` validates the request against its issued invite record,
   then appends the joiner's ticket and fact-author key to local product state.
5. When both daemons run, each node has the peer address and author trust needed
   for p2panda-net fact sync to import verified membership facts.

Tickets are generated from the node's persisted p2panda seed and persisted bind
port instead of from a live daemon endpoint. This avoids the previous
ephemeral-ticket failure where every daemon restart invalidated bootstrap
coordinates.

## Verification

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport`
- `cargo clippy --manifest-path MVP/Cargo.toml -p mvp-node --all-targets -- -D warnings`

