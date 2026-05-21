---
title: Slice 050 Transitive Peer Authority Report
status: complete
created: 2026-05-19
plan: MVP/slice-050-transitive-peer-authority-plan.md
---

# Slice 050 Transitive Peer Authority Report

Slice 050 closes the product membership gap that kept the three-server vertical
from being more than a two-node bootstrap demo.

## What Changed

- Added `PeerAdmittedFact` and `/facts/peer/<node_id>/admitted/<epoch>` key
  classification/validation.
- The product daemon now republishes durable admitted-peer facts for its accepted
  joiners.
- Nodes consume imported admitted-peer facts, persist the peer in local node
  state, trust the peer fact author, grant product fact access, and add the
  peer's p2panda transport ticket at runtime.
- Added async candidate/payload read helpers on `SharedPandaFactStore` so live
  daemon code does not rely on the synchronous `FactSource` adapter's
  nonblocking lock behavior.
- Replaced the two-node product convergence test with a three-node A/B/C
  convergence proof over p2panda-net.

## Important Boundary

Peer-admitted facts are authority propagation evidence, not projected active
membership. Operator-visible membership remains the reduced node join/remove
fact set. This keeps transport trust installation out of the membership reducer
and leaves room for a later p2panda-auth membership replacement without changing
the product CLI shape.

## Proof

The focused product proof now initializes A, joins B and C through A, admits
both, runs all three daemons, and verifies that A, B, and C each project exactly
three joined nodes from their local p2panda stores.

## Next Blocker

The three-server product vertical can now move from membership to node-agent
services: long-running daemon-owned participants that respond to inspect,
prepare, start, drain, and stop requests using the existing bus/deploy
semantics.

