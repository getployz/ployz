---
title: Slice 046 p2panda-net Fact-node Reliability
status: completed
created: 2026-05-19
origin:
  - MVP/slice-046-p2panda-net-fact-node-reliability-plan.md
  - MVP/design-notes/p2panda-substitution-gains.md
  - MVP/primitive-decisions.md
external:
  - https://docs.rs/p2panda-net/latest/p2panda_net/
  - https://docs.rs/p2panda-sync/latest/p2panda_sync/
---

# Slice 046 p2panda-net Fact-node Reliability

## Result

The zero-import class from Slice 045 now has a focused reliability gate.

`p2panda-net-fact-node-reliability-contract` runs 12 canonical
`PandaNetFactNode` roundtrips in one bounded scenario. Each iteration publishes
one valid fact operation over p2panda-net, imports through the same
`SharedPandaFactStore` authority path as the product proofs, and fails if the
receiver reaches its deadline with zero attempted imports after publish.

The first run passed:

```text
iterations: 12
zero_import_iterations: 0
total_attempted_imports: 12
total_imported_operations: 12
total_idle_timeouts: 1
total_stream_refreshes: 1
startup_p95_ms: 24
import_p95_ms: 74
elapsed_ms: 1590
```

The original `p2panda-net-fact-node-contract` also now reports import-loop
diagnostics. Its validation run passed with 11 attempted imports, 8 inserted
operations, 1 conflict, 2 structured rejections, 1 idle refresh, and no
stream-ended/lagged/failed events.

The process-serving proof now reports the same no-progress class from the
separate serving/projection role. Its validation run passed with 3 attempted
import batches, 2 imported serving updates, 1 rejected untrusted-author
operation, 2 idle refreshes, 2 stream refreshes, no refresh failures, and
unchanged last-good gateway/DNS behavior while the local coordinator socket was
absent.

## What Changed

- `PandaNetFactNode` exposes `PandaNetFactNodeStats` with idle timeout, stream
  refresh, stream error, replay skip, and sync lifecycle counters.
- `PandaNetFactNode::add_node_info` inserts a peer into the p2panda-net address
  book after spawn. The E2E harness uses this to make direct two-node
  reliability tests explicitly bidirectional instead of relying on one-way
  bootstrap timing.
- `PandaNetFactNode::refresh_publish_stream` lets long-lived publishers reopen
  their publish stream before delayed publishes. The publish path also retries
  once after a publish-stream error.
- `p2panda-net-fact-node-contract` records the new diagnostics in its metrics
  and includes them in failure messages.
- `p2panda-net-fact-node-reliability-contract` exercises repeated canonical
  fact-node transport and fails on zero-attempt iterations.
- `p2panda-net-process-serving-contract` exposes attempted import batches and
  the same p2panda-net stream counters through `P2pandaNetRoleStatus`.

## Decision

Keep `PandaNetFactNode` as the product-shaped p2panda-net fact transport.

The new proof did not justify replacing p2panda-net with direct
`p2panda-sync`, a custom iroh sync loop, or another transport wrapper. The
useful fix was to make peer address-book insertion explicit where the harness
has both node identities, and to report the no-progress counters instead of
collapsing every wait into "imports did not arrive."

p2panda-net address book/discovery remains transport observation. It is not
membership truth, command consistency, or a quorum substitute.

## Semantic Leverage

This is not a raw LOC reduction slice. It adds test and status surface.

The leverage is that future product slices can trust one existing fact
transport boundary instead of reintroducing a feature-local sync path. Volume
transfer should now be able to move to membership-backed p2panda facts without
inventing another transport or manual replay harness.

## Proofs

Commands run:

```text
cargo check --manifest-path MVP/Cargo.toml -p mvp-e2e
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-reliability-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-process-serving-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
```

Outcomes:

- `mvp-p2panda-transport`: 15 tests passed.
- `p2panda-net-fact-node-reliability-contract`: passed, 12 iterations, 12
  attempted imports, 12 imported operations, zero zero-import iterations.
- `p2panda-net-fact-node-contract`: passed, including projection rebuild,
  gateway/DNS snapshots, and HTTP-01 200-before-clear/404-after-clear behavior.
- `p2panda-net-process-serving-contract`: passed, including delayed remote
  serving update, rejected untrusted author, rebuild, restart, and absent local
  coordinator socket.
- Simplify pass: removed the duplicate replay-cache skip counter after
  `PandaNetFactNodeStats` became the single replay-skip reporting surface.
  After that simplification, `cargo check -p mvp-e2e`,
  `cargo test -p mvp-p2panda-transport`,
  `p2panda-net-fact-node-reliability-contract`, and
  `p2panda-net-process-serving-contract` passed again.
- Full-suite rerun then exposed a process-serving transport failure where the
  delayed second scripted publish did not arrive after a receiver stream
  failure. The fix added publish-stream refresh before delayed scripted
  publishes and one publish retry after a publish-stream error.

## Review

Subagent review was attempted with correctness, reliability, and testing
reviewers. All three agents hit the account usage limit, so I ran the review
and simplify pass locally instead.

Local review focus:

- Correctness: the new counters must observe behavior without changing import
  classification.
- Reliability: the repeated proof must fail on zero attempted imports rather
  than hiding the race behind sleeps.
- Maintainability: the process-role status should stay a status surface, not a
  second transport policy layer.

The only simplification finding was the duplicate replay-skip counter described
above.

## Next Slice

Volume transfer membership-backed facts is the next product slice. It is now
the last product-shaped manual-trust canary, and this slice gives it a stronger
p2panda-net fact transport proof to build on.
