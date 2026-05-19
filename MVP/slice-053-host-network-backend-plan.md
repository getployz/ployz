---
title: Slice 053 Host Network Backend Plan
status: implemented
created: 2026-05-19
origin:
  - MVP/three-server-product-vertical-plan.md
  - MVP/slice-052-minimal-process-runtime.md
---

# Slice 053 Host Network Backend Plan

## Goal

Make node-to-node service endpoints concrete enough for the three-server product
vertical without blocking on privileged kernel WireGuard mutation.

## Scope

- Add a typed host-network snapshot to `mvp-mesh`.
- Convert projected gateway backend addresses into typed `SocketAddr` values.
- Validate active backend reachability with a bounded TCP connect.
- Persist the last applied host-network snapshot under node state.
- Add a small node-level wrapper so product code can apply and reload the
  snapshot through `LoadedNodeState`.

## Non-Goals

- Kernel or userspace WireGuard interface mutation.
- Firewall, routing table, DNS, or container network setup.
- Cross-cloud private-network provisioning.
- Service discovery beyond addresses already produced by projection.

## Decision

The first three-server proof will use host-routable addresses. That is enough to
prove the product vertical on three servers with an existing private network or
public test addresses. WireGuard remains the intended private data plane, but
its Linux adapter should land behind the existing backend boundary after the
product deploy path is black-box testable.

## Proof

- A projected gateway route becomes a typed host-network snapshot.
- Invalid backend address strings are rejected before apply.
- Applying a snapshot probes backend reachability and records the last applied
  state atomically.
- A fresh backend value can reload the applied state, proving the snapshot is not
  tied to daemon process memory.

## Verification

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-mesh`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `cargo clippy --manifest-path MVP/Cargo.toml -p mvp-mesh -p mvp-node --all-targets -- -D warnings`
