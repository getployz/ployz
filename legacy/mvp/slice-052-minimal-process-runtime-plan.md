---
title: Slice 052 Minimal Process Runtime Plan
status: implemented
created: 2026-05-19
origin:
  - MVP/three-server-product-vertical-plan.md
  - MVP/slice-051-node-agent-services.md
---

# Slice 052 Minimal Process Runtime Plan

## Goal

Replace the node-agent's purely in-memory start/stop proof with a minimal real
process backend that can run a trivial stateless HTTP service and survive daemon
restart through persisted metadata.

## Scope

- Add a new `mvp-runtime` crate.
- Persist per-instance process metadata below the node state directory.
- Prepare a simple document root for each instance.
- Start a managed static HTTP service process.
- Wait for readiness before returning `Ready`.
- Mark drain as runtime state without killing the process.
- Stop the process by PID.
- Let restarted runtime code rediscover instance metadata.
- Keep `mvp-node` as the shipped binary by adding a hidden `runtime-http` child
  role used by the process backend.

## Non-Goals

- Container runtime.
- Supervisor restart policy.
- cgroups, namespaces, environment files, volumes, or logs.
- Production HTTP server behavior for the test service.
- Cross-node product deploy command wiring.

## Proof

- `mvp-runtime` starts a child HTTP process, reads a response, drains without
  stopping it, rediscovers metadata through a fresh runtime value, and stops it.
- `mvp-node` node-agent integration starts the shipped-binary `runtime-http`
  role through the deploy start handler and stops it through the deploy stop
  handler.

## Verification

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-runtime -- --nocapture`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node product_node_agent_starts_http_process_with_shipped_binary_role -- --nocapture`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `cargo clippy --manifest-path MVP/Cargo.toml -p mvp-runtime -p mvp-node --all-targets -- -D warnings`

