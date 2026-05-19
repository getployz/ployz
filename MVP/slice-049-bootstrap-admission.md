---
title: Slice 049 Bootstrap Admission
status: complete
created: 2026-05-19
plan:
  - MVP/slice-049-bootstrap-admission-plan.md
---

# Slice 049 Bootstrap Admission

## What Changed

- Added stable p2panda ticket generation from persisted node seed and bind port.
- Added issued invite records under bootstrap node state.
- Persisted joined-node invite credentials so `admission` can be produced after
  `join` without passing the original token again.
- Added `AdmissionRequest` / `AdmissionReport` and product commands:
  - `mvp-node admission --state <joined-node>`
  - `mvp-node admit --state <bootstrap-node> --request <json>`
- Added durable bootstrap peer recording that appends the admitted node's ticket
  and trusted fact author to product state.
- Kept admitted peer state as a single record tying node id, principal, author
  key, invite id, and p2panda ticket together, rather than parallel trust/address
  lists.
- Moved issued-invite persistence behind the node state boundary with atomic
  writes.
- Validated join-derived state before writing `node-state.json`, so malformed
  non-expired invites cannot poison an empty state directory.
- Rejected admission requests whose principal does not match `node:<node_id>` or
  that conflict with an existing admitted peer.
- Made bounded daemon import loops refresh recoverable p2panda stream failures
  and surface refresh failures instead of reporting healthy.

## Proof

The product membership test now covers:

1. Node A initializes and creates an invite without requiring a daemon to run.
2. Node B joins from that invite.
3. Node B emits an admission request.
4. Node A admits Node B and persists reciprocal peer trust/address data.
5. Both product daemons run and converge to two projected membership facts via
   p2panda-net, not direct store import.

Verification run:

```text
cargo test --manifest-path MVP/Cargo.toml -p mvp-node
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-node --all-targets -- -D warnings
```

## Remaining Gap

This proves bootstrap-to-one-joiner convergence. The full three-server vertical
still needs transitive authority propagation: when node C is admitted by node A,
node B must learn node C's author key and node C must learn node B's author key
through durable facts. That should be the next membership slice before moving
to node-agent deploy services.
