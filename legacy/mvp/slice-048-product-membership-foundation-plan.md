---
title: Slice 048 Product Membership Foundation Plan
status: implemented
created: 2026-05-19
origin:
  - MVP/three-server-product-vertical-plan.md
  - MVP/node/src/main.rs
---

# Slice 048 Product Membership Foundation Plan

## Goal

Wire the first product-facing membership surface into `mvp-node` without
touching the legacy codebase: bounded daemon runs, invite token creation, joined
node initialization, durable self-join facts, and reopenable projection evidence.

## Scope

In scope:

- `mvp-node daemon --state ... --run-for-ms ...`
- `mvp-node invite --state ...`
- `mvp-node join --state ... --token ...`
- persisted bootstrap p2panda ticket and bootstrap author trust
- local product test proving daemon-written join facts survive process restart

Out of scope for this slice:

- bootstrap admission RPC
- reciprocal p2panda address-book updates
- three-node convergence
- deploy/node-agent services

## Implementation Notes

The important design correction from this slice is that a join token cannot only
carry transport coordinates. The joining node also needs the bootstrap node's
fact-author principal and public key so imported bootstrap facts can verify
against the same authority rules used by the persistent fact store.

This still does not solve the reverse direction. The bootstrap daemon must learn
the joiner's p2panda node info and fact-author key through an explicit admission
request before full membership convergence can be a product proof.

## Verification

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`

