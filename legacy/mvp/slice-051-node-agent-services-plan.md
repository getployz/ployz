---
title: Slice 051 Node Agent Services Plan
status: implemented
created: 2026-05-19
origin:
  - MVP/three-server-product-vertical-plan.md
  - MVP/slice-050-transitive-peer-authority.md
---

# Slice 051 Node Agent Services Plan

## Goal

Move the three-server product vertical from converged membership to daemon-owned
deploy participant services. The product daemon should register node-local
handlers that use the existing `mvp-deploy` wire contracts instead of test-local
closures.

## Scope

- Register capacity, prepare, start, drain, stop, and candidate-cleanup
  participant subjects for the local node.
- Keep runtime state below the daemon boundary, even though the first runtime is
  in-memory.
- Prove the handlers through `BusActorHandle` requests with real deploy wire
  payloads.
- Prove authorization denial through bus grants.
- Report node-agent handler registration from the daemon run.

## Non-Goals

- Real process/container runtime. That is U4.
- Cross-node product bus transport. This slice keeps the local bus participant
  contract ready for that transport.
- Product `deploy` command wiring.
- Gateway/DNS projection changes.

## Proof

- A node-agent services test requests capacity, prepare, start, drain, and stop
  through the bus and observes typed replies plus local runtime state changes.
- A restart-shaped test reloads node state and recreates service registrations.
- An auth test verifies a principal without node RPC grants cannot invoke the
  node-agent.
- Candidate cleanup removes local candidate runtime state.

## Verification

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node node_agent -- --nocapture`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `cargo clippy --manifest-path MVP/Cargo.toml -p mvp-node --all-targets -- -D warnings`

