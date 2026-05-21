---
title: Slice 054 Product Deploy Command Plan
status: implemented
created: 2026-05-19
origin:
  - MVP/three-server-product-vertical-plan.md
  - MVP/slice-053-host-network-backend.md
---

# Slice 054 Product Deploy Command Plan

## Goal

Wire a product-shaped deploy path through the primitives built so far: node
agent services, process runtime, deploy coordinator, p2panda-backed fact
writers, projection, and host-network apply.

## Scope

- Let deploy participant start replies carry the concrete backend endpoint
  allocated by the runtime.
- Materialize serving commit active backends from participant replies when the
  runtime reports them.
- Add `mvp-node deploy` for a simple one-service deploy.
- Use persistent p2panda deploy and serving fact writers.
- Project facts into SQLite plus gateway/DNS snapshots.
- Apply host-network reachability after projection catch-up.
- Prove first deploy and update deploy with real process runtime.

## Non-Goals

- Cross-process bus transport for remote node-agent RPC.
- Production manifest TOML.
- Gateway/DNS process-role CLI wiring.
- Black-box three-server smoke script.

## Proof

- First deploy starts a shipped-binary HTTP child process and projects reachable
  backend state.
- Update deploy starts a new backend, projects the route change, and drains plus
  stops the old backend only after projection catch-up.

## Verification

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node --test product_deploy -- --nocapture`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `cargo clippy --manifest-path MVP/Cargo.toml -p mvp-deploy -p mvp-node --all-targets -- -D warnings`
