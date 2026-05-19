---
title: Slice 047 Product Binary State Plan
status: completed
created: 2026-05-19
origin:
  - MVP/three-server-product-vertical-plan.md
---

# Slice 047 Product Binary State Plan

## Proof Target

Create the first real MVP product binary and persistent state layout. The slice
is deliberately narrow: prove `init` and `status` round-trip durable node
identity and state paths without borrowing `mvp-e2e` role wiring as the product
surface.

## Scope

In scope:

- new `mvp-node` workspace crate,
- `init --state <dir> [--island <id>] [--node-id <id>]`,
- `status --state <dir>`,
- persistent node state JSON,
- persisted island id, node id, principal id, p2panda author key, p2panda-net
  network/node/topic seeds, and WireGuard identity placeholder,
- derived fact/projection and snapshot paths from the requested state directory,
- explicit not-wired errors for future product commands.

Out of scope:

- real invite/join transport,
- daemon role,
- node-agent services,
- runtime backend,
- WireGuard apply,
- deploy command.

## Verification

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `cargo check --manifest-path MVP/Cargo.toml -p mvp-e2e`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-node -- init --state <tmp> --island prod --node-id node-a`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-node -- status --state <tmp>`

## Follow-Up

Slice 048 should wire `invite` and `join` through product state and
p2panda-net-backed membership facts. The `mvp-node` crate should remain a thin
composition layer; membership/admission semantics stay in `mvp-mesh`, authority
in p2panda membership/facts, and transport mechanics in `mvp-p2panda-transport`.
