---
title: Slice 046 p2panda-net Fact-node Reliability Plan
status: completed
created: 2026-05-19
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution-gains.md
  - MVP/slice-045-p2panda-substitution-gains.md
external:
  - https://docs.rs/p2panda-net/latest/p2panda_net/
  - https://docs.rs/p2panda-sync/latest/p2panda_sync/
---

# Slice 046 p2panda-net Fact-node Reliability Plan

## Problem Frame

Slice 045 found one focused `p2panda-net-fact-node-contract` failure where the
receiver reported zero attempted imports, followed by a clean immediate rerun.
That failure is small but important: the p2panda-net fact node is becoming the
MVP's product-shaped transport for signed fact operations, so a zero-import
false failure has to be explained before volume transfer, machine membership,
or more serving-state proofs depend on it.

This slice is not a p2panda-net rewrite. It is a bounded reliability and
observability slice. The goal is to make the fact-node E2E tell us whether
delivery failed, the stream had not reached live mode, the wrapper refreshed at
the wrong time, replay suppression skipped already-seen operations, or the
harness observed too early.

The plan keeps the current architecture boundary:

- p2panda-net owns iroh transport, address book, discovery, gossip, topic log
  sync, and optional internal supervision.
- Ployz owns fact authorization, import outcomes, projection reducers,
  last-good serving state, command consistency, and operator-visible failure.
- p2panda discovery/address-book state is transport observation, not durable
  membership or command truth.

## Research Notes

The current `p2panda-net 0.6.0` docs describe the crate as
data-type-agnostic peer-to-peer networking, discovery, gossip, and local-first
sync. Its module list includes iroh endpoints, confidential topic discovery,
gossip, `LogSync`, address book, and optional supervisor-backed modules.

The docs also make the key reliability distinction relevant to this slice:
gossip is for online ephemeral delivery, while eventual catch-up requires a
sync protocol. The MVP fact node already uses p2panda-net `LogSync` with
canonical `Operation<PandaFactExtensions>` values, so the implementation should
debug our wrapper and harness around that stream before building another
transport layer.

`p2panda-sync` remains useful as the lower-level sync protocol boundary, but
this slice should stay on the live p2panda-net path. Falling back to direct
store-to-store sync would prove the wrong thing.

## Scope

In scope:

- Add focused import-loop observation around `PandaNetFactNode` and the E2E
  helpers that consume it.
- Add a repeated, bounded reliability E2E that exercises canonical
  p2panda-net fact-node transport enough times to catch zero-import false
  failures.
- Preserve the process-serving p2panda-net proof and surface the same class of
  status in the serving/projection role path.
- Improve failure messages so a future failure reports idle refreshes,
  stream-ended refreshes, replay skips, attempted imports, terminal outcomes,
  and last transport/import error.
- Keep all changes under `MVP/`.

Out of scope:

- Do not migrate volume transfer in this slice.
- Do not replace p2panda-net with direct `p2panda-sync` or a custom iroh sync
  loop.
- Do not use p2panda address book, discovery, or supervisor state as Ployz
  membership truth.
- Do not delete manual trust fallback probes yet.
- Do not broaden this into generic process-role supervision.

## Design Decisions

### Add Observation Before Policy

The failure we saw had `attempted: 0`, so the missing evidence is before import
classification. The first implementation pass should record what happened while
waiting for a stream operation:

- idle timeouts,
- stream-ended refreshes,
- stream refresh attempts and failures,
- replayed operation skips,
- non-operation sync events observed if the wrapper exposes them,
- last transport error,
- elapsed time from node spawn to first attempted import.

This can be E2E-local if that keeps the production API small. Promote a
transport-level stats type only if the process role also needs the same data.

### Keep Refresh Bounded And Explicit

`PandaNetFactNode::refresh_stream` is already part of the current recovery
shape. The slice should make refresh behavior visible rather than adding
unbounded retries or sleeps. Waits must be deadline-driven and should fail with
a structured report when no import was attempted after facts were published.

### Repeated E2E Is The Proof

A single rerun already proved the bug is intermittent. The slice needs one
bounded repeated scenario that fails on any zero-attempt iteration. The scenario
should use the canonical fact-node path, not shell out to existing scenarios.
It can reuse helper setup from `p2panda_net_fact_node_contract.rs`, extracting
small shared helpers only when that removes real duplication.

### Process-role Proof Should Report The Same Class

`process_role_harness.rs` already counts some p2panda-net serving import-loop
state. The slice should extend or normalize that status so
`p2panda-net-process-serving-contract` can report the same basic reliability
signals. Do not turn placeholder HTTP/DNS serving into production polish; focus
on the surviving serving actor, snapshot, last-good, and fact import surfaces.

## Implementation Units

### Unit 1: Import-loop Diagnostics

Files:

- `MVP/p2panda-transport/src/fact_node.rs`
- `MVP/p2panda-transport/src/errors.rs`
- `MVP/p2panda-transport/src/lib.rs`
- `MVP/e2e/src/p2panda_net_fact_node_contract.rs`
- `MVP/e2e/src/process_role_harness.rs`

Plan:

1. Inspect the current stream event handling in `PandaNetFactStream`.
2. Add the narrowest report/status type needed to distinguish no-progress
   waits from terminal import outcomes.
3. Count idle timeouts, stream-ended refreshes, replay skips, refresh failures,
   and last failure in fact-node E2E wait helpers.
4. If the transport wrapper can expose non-operation `TopicLogSyncEvent`
   observations without complicating the public API, include counts for sync
   start/finish/live-mode style events. Otherwise leave that as an explicitly
   documented future probe.

Test Scenarios:

- A normal import path records at least one attempted import and zero
  no-progress terminal failures.
- A stream-ended event followed by refresh is visible in the report rather than
  swallowed.
- Duplicate/replay suppression still avoids inflating duplicate import counts.

### Unit 2: Repeated Fact-node Reliability E2E

Files:

- `MVP/e2e/src/p2panda_net_fact_node_reliability_contract.rs`
- `MVP/e2e/src/p2panda_net_fact_node_contract.rs`
- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/metrics.rs`

Plan:

1. Add `p2panda-net-fact-node-reliability-contract`.
2. Run a bounded number of fact-node roundtrips in one scenario. Start with a
   count that keeps `mvp-e2e -- all` within its wall-clock budget; increase only
   if runtime evidence says the budget can absorb it.
3. For each iteration, publish a small known set of canonical fact operations
   and import until terminal expected outcomes are reached.
4. Fail immediately if an iteration reaches its deadline with zero attempted
   imports after publish.
5. Write a metrics report with per-iteration and aggregate values:
   attempted imports, inserted imports, conflicts, rejections, idle refreshes,
   stream-ended refreshes, replay skips, startup milliseconds, sync/import
   milliseconds, and zero-import iterations.

Test Scenarios:

- Every iteration imports the expected valid fact operations.
- Every iteration observes the expected conflict/rejection outcomes when those
  probes are included.
- `zero_import_iterations == 0`.
- The scenario failure message names the first failed iteration and its import
  wait report.

### Unit 3: Process-serving Reliability Status

Files:

- `MVP/e2e/src/process_role_harness.rs`
- `MVP/e2e/src/p2panda_net_process_serving_contract.rs`
- `MVP/e2e/src/metrics.rs`

Plan:

1. Extend the process role status or metrics to include the same no-progress
   counters used by the direct fact-node reliability scenario.
2. Keep the serving process path product-shaped: a separate serving/projection
   role receives p2panda-net fact traffic, imports authorized operations,
   projects gateway/DNS state, and serves last-good snapshots without a local
   coordinator socket.
3. Add assertions that the serving process did not depend on zero-attempt
   import loops to pass.

Test Scenarios:

- `p2panda-net-process-serving-contract` still proves delayed update delivery
  from a stable publisher peer.
- Serving role status reports import attempts and no zero-import terminal
  failure.
- Existing last-good gateway/DNS behavior remains unchanged.

### Unit 4: Documentation And Slice Report

Files:

- `MVP/slice-046-p2panda-net-fact-node-reliability.md`
- `MVP/overall-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/e2e-proof-plan.md`
- `MVP/design-notes/p2panda-substitution-gains.md`
- `MVP/design-notes/semantic-leverage-loc.md`

Plan:

1. Record whether the fix was observation-only, wrapper hardening, harness
   correction, or a p2panda-net behavior workaround.
2. Update `primitive-decisions.md` only with material decisions, not every
   counter added for test reporting.
3. Update the E2E proof plan with the repeated reliability scenario and any
   `all` budget impact.
4. Record semantic-leverage impact: whether p2panda-net absorbed transport
   mechanics cleanly, or whether Ployz still owns too much retry/status glue.

## Verification

Run before implementation commit:

```text
cargo check --manifest-path MVP/Cargo.toml -p mvp-e2e
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-process-serving-contract
```

Run before slice completion:

```text
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-reliability-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
git diff --check
```

The reliability scenario must be time-budgeted. If the repeated proof makes
`all` materially slower, reduce iteration count first and document the limit.
Do not keep the scenario outside `all` unless the report explains why the full
suite cannot afford it yet.

## Review Focus

Use subagents for the implementation review when available:

- Correctness: no hidden sleep-based masking of stream races; deadline-driven
  waits and clear no-progress errors.
- Reliability: process-role serving keeps last-good state and exposes import
  health without promoting transport observations into durable truth.
- Maintainability: avoid creating another generic retry framework. The slice
  should make the current fact-node boundary simpler to trust.
- Testing: repeated proof catches the exact zero-import class that Slice 045
  observed.

If the subagent usage limit is still active, run the same review locally and
record that limitation in the slice report.

## Exit Criteria

- The focused zero-import class has a reproducible reliability gate.
- The new gate fails on zero attempted imports after publish.
- Direct fact-node and process-serving p2panda-net proofs still pass.
- `mvp-e2e -- all` remains within its budget or the budget change is explicit.
- The slice report explains what changed and whether volume transfer can safely
  proceed next.

## Expected Follow-up

If this slice stabilizes p2panda-net fact-node transport, Slice 047 should move
`volume-transfer-contract` onto membership-backed p2panda facts and delete the
last product-shaped manual trust canary.
