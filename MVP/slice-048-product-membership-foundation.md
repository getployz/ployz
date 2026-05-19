---
title: Slice 048 Product Membership Foundation
status: complete
created: 2026-05-19
plan:
  - MVP/slice-048-product-membership-foundation-plan.md
---

# Slice 048 Product Membership Foundation

## What Changed

- Added product `invite`, `join`, and bounded `daemon` command wiring to
  `mvp-node`.
- Added persistent bootstrap ticket handling in the node state directory.
- Added invite tokens that carry p2panda network/topic, bootstrap node ticket,
  invite metadata, and bootstrap fact-author identity.
- Added joined-node initialization that persists bootstrap ticket and bootstrap
  fact-author trust.
- Split root initialization from joined-node initialization so join-only
  bootstrap state is required as a complete typed input instead of optional
  fields on normal `init`.
- Validate persisted bootstrap tickets and trusted author keys at the state
  boundary on both init and load.
- Added daemon startup that opens the persistent p2panda fact store, writes a
  dialable local node ticket for local runs, publishes the node's self-join fact,
  and imports available p2panda fact batches for the bounded run window.

## Proof

`mvp-node` now has product-level tests for:

- invite failure before a daemon has produced a bootstrap ticket,
- token-based join initialization with inherited p2panda network/topic,
- expired invite rejection before state mutation,
- daemon-written self-join facts that can be projected after daemon exit.

Verification run:

```text
cargo test --manifest-path MVP/Cargo.toml -p mvp-node
```

## Remaining Gap

This slice deliberately does not pretend product membership convergence exists.
The next slice needs bootstrap admission: the joining daemon sends its node info,
principal, fact-author key, WireGuard identity, and invite proof to the
bootstrap daemon; the bootstrap daemon validates the invite, records reciprocal
trust/address-book state, and writes the admitted membership facts.
