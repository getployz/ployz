---
title: Slice 050 Transitive Peer Authority Plan
status: implemented
created: 2026-05-19
origin:
  - MVP/three-server-product-vertical-plan.md
  - MVP/slice-049-bootstrap-admission.md
---

# Slice 050 Transitive Peer Authority Plan

## Goal

Make product membership transitive enough for the three-server vertical:
bootstrap node A admits nodes B and C, then B and C learn each other's fact
authors through replicated cluster facts instead of manual local state edits.

## Problem

Slice 049 proved only a two-node path. Node B trusted A from its invite token,
and A trusted B from admission. When A later admitted C, B had no authority
evidence for C, so it could receive C's transport traffic but could not import
C-authored membership facts. The three-server product path needs admitted peer
authority to propagate as durable truth.

## Design

- Add a durable `/facts/peer/<node_id>/admitted/<epoch>` fact.
- The admitting node writes this fact for each admitted peer it knows.
- The fact carries the admitted node id, principal id, p2panda author key,
  p2panda ticket, invite id, and epoch.
- Already-joined nodes scan replicated peer-admission facts, validate the fact
  payload against the key, persist the peer in local node state, trust the
  peer's author key, grant product fact read/write authority, and add the
  peer's p2panda node info to the live transport node.
- Projection reducers classify and validate the fact shape but do not project it
  into operator membership; operator-visible membership still comes from
  node-joined/tombstone facts.

## Non-Goals

- Full p2panda-auth membership replacement.
- Strong-removal or hostile-node exclusion changes.
- Active-member/quorum/partition semantics.
- Packaging or long-running service manager work.

## Proof

Replace the two-node product convergence proof with a three-node test:

1. Node A initializes.
2. Node A invites B and C.
3. B and C join from A's tokens.
4. A admits B and C.
5. A, B, and C run bounded product daemons concurrently.
6. All three local p2panda stores reduce to exactly three joined nodes.

## Verification

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node three_admitted_product_daemons_converge_join_facts_over_p2panda_net -- --nocapture`
- `cargo clippy --manifest-path MVP/Cargo.toml -p mvp-node --all-targets -- -D warnings`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-projection`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport`

