---
title: "feat: MVP bus scale runtime and metrics"
type: feat
status: active
date: 2026-05-17
origin:
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/slice-001-bus-e2e-contract-plan.md
  - MVP/slice-001-bus-contract.md
---

# feat: MVP bus scale runtime and metrics

## Summary

Build the next implementation slice entirely under `MVP/`: harden the
PloyzBus runtime enough that the scale harness is meaningful, introduce the
first actor-owned bus boundary, and implement the `scale` E2E scenario promised
by the MVP proof plan.

The proof target is not a deterministic simulator. It is an executable
logical-node stress harness that exercises real bus publish and request-many
paths at 200, 1,000, and 10,000 logical subscribers/responders, records latency
and memory metrics, and keeps the code easy for future deploy/machine logic to
use.

## Problem Frame

Slice 001 proved core bus semantics but intentionally left runtime and scale
work shallow. The review findings are now the next architectural risk:

- delivery workers are bounded per dispatch, not across the bus,
- payloads are cloned per delivery,
- queue dispatch and routing scans are simple but not yet measured,
- scale metrics are counts and elapsed time only,
- `cargo run -p mvp-e2e -- scale` is still not implemented.

Every later slice uses the bus for facts, projections, membership, deploy, and
gateway/DNS reloads. If the bus substrate cannot provide clear backpressure,
bounded concurrency, shared payloads, and real metrics, later E2E failures will
be ambiguous.

## Success Criteria

- All new code stays under `MVP/`.
- `cd MVP && cargo run -p mvp-e2e -- scale` runs successfully.
- `cd MVP && cargo run -p mvp-e2e -- all` runs both bus contract and scale
  scenarios.
- Scale output includes structured JSON metrics for 200, 1,000, and 10,000
  logical-node runs.
- Metrics include at least publish p50/p95/p99, request-many p50/p95/p99,
  subscriber/responder counts, payload bytes, expected versus observed replies,
  max worker concurrency, and process memory before/after.
- Bus delivery execution is bounded at the bus/runtime level, not per dispatch.
- Payload fanout no longer deep-clones a `Vec<u8>` per subscriber.
- The bus has a first actor-owned boundary so future subsystem code does not
  grow direct access to bus internals.
- Existing slice 001 contract behavior stays green.
- A slice note records semantic leverage and simplicity observations.

## Scope

In scope:

- MVP-local dependency additions in `MVP/Cargo.toml` and MVP-local crate
  manifests only.
- Bus runtime refactor in `MVP/bus`.
- A Kameo-owned bus facade or runtime actor that owns the bus boundary.
- Shared payload representation.
- Bus-wide delivery worker bound.
- Scale scenario and metrics helpers under `MVP/e2e`.
- Unit tests for runtime/backpressure/drain behavior introduced by this slice.
- E2E scale metrics artifact under `MVP/target/mvp-e2e/`.

Out of scope:

- iroh transport.
- iroh-docs, iroh-blobs, facts, projections, gateway/DNS snapshots.
- Any existing `crates/` code or root workspace wiring.
- Optimized wildcard indexes unless the scale run proves the simple scan is a
  blocker.
- Production benchmarking claims. This slice produces MVP proof metrics, not a
  release benchmark suite.
- Process-level multi-daemon tests.

## Crate Scout

Checked before planning:

- `kameo` 0.20.0: docs describe async actors, lifecycle hooks, bounded or
  unbounded mailboxes, and supervision. Adopt for the first bus actor boundary
  because actor ownership is a core MVP design goal.
  <https://docs.rs/kameo/latest/kameo/actor/index.html>
- `kameo::mailbox`: docs expose bounded mailboxes for actor backpressure.
  Use this for actor-level acceptance pressure rather than open-ended message
  queues. <https://docs.rs/kameo/latest/kameo/mailbox/index.html>
- `crossbeam-channel`: docs support bounded/unbounded channels, cloned
  senders/receivers, blocking operations, timeouts, and selection. Adopt for a
  tiny bus-owned worker pool if Kameo alone does not fit handler fanout
  execution; it avoids writing an MPMC queue by hand.
  <https://docs.rs/crossbeam/latest/crossbeam/channel/index.html>
- `bytes`: docs describe efficient byte buffers. Adopt `Bytes` or a small
  `Payload` newtype backed by it so fanout clone cost is reference-counted
  instead of copying every byte for every subscriber.
  <https://docs.rs/bytes/latest/index.html>
- `hdrhistogram`: docs expose latency histograms and percentile/quantile
  queries. Adopt for E2E metrics because p50/p95/p99 are acceptance criteria.
  <https://docs.rs/hdrhistogram/latest/hdrhistogram/struct.Histogram.html>
- `memory-stats`: docs expose current-process memory statistics. Prefer this
  small crate for the MVP scale harness; fall back to `sysinfo` only if it
  cannot provide stable RSS-like values on the local target.
  <https://docs.rs/memory-stats/latest/memory_stats/struct.MemoryStats.html>
- `criterion`: strong for microbenchmarks, but not adopted for this slice
  because the MVP requirement is an executable E2E/stress scenario with
  scenario-shaped metrics, not a benchmark harness.
  <https://docs.rs/criterion/latest/criterion/>

Decision: adopt Kameo, shared payload storage, histogram metrics, and a small
process-memory metric dependency. Use `crossbeam-channel` only for a simple
bus-wide worker pool if the Kameo actor facade does not directly solve handler
fanout concurrency.

## Key Decisions

1. The scale harness is the next best proof target.
   It reduces uncertainty before facts/projections/deploy build on top of the
   bus. It also directly addresses the user's requirement that the MVP be
   stress-tested rather than functionality-reduced.

2. Actor ownership starts here, but transport stays out.
   The bus actor should own local bus state or the public bus facade. It should
   not introduce iroh, docs, blobs, WireGuard, or gateway concerns.

3. Keep business semantics ergonomic.
   Future deploy code should still say "request many capacity responders" or
   "publish gateway changed" without managing worker pools, payload sharing,
   histograms, or actor mailboxes.

4. Scale assertions are explicit gates.
   The harness should fail when replies are missing, fanout counts are wrong,
   metrics cannot be written, or the 10,000-node run exceeds deliberately
   chosen safety thresholds.

5. Avoid production sleeps in tests.
   Prior learning from `docs/solutions/performance-issues/machine-add-timeout-tests-2026-05-10.md`
   applies here: tests should exercise timeout behavior with operation-scoped
   short deadlines, not wall-clock production waits.

6. Drain remains foreground and audience-aware.
   Prior learning from `docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md`
   applies here: local state mutation and remote-style dispatch must stay
   distinguishable, and drain behavior needs direct tests.

## Implementation Units

### U1. Shared payload and bus runtime configuration

Files:

- Modify `MVP/bus/Cargo.toml`
- Modify `MVP/bus/src/message.rs`
- Modify `MVP/bus/src/memory.rs`
- Modify `MVP/bus/src/lib.rs`

Approach:

- Replace `Payload = Vec<u8>` with a small ergonomic payload type backed by
  shared immutable bytes.
- Preserve simple call sites with `From<Vec<u8>>`, `From<&'static [u8]>`, and
  byte-slice accessors.
- Add runtime configuration for worker count and queue/backpressure limits.
- Keep slice 001 semantics unchanged.

Test scenarios:

- Payload clones share backing storage rather than copying data.
- Existing request/reply and publish tests still pass with the new payload
  type.
- Large payload fanout does not allocate one full copy per subscriber.

Verification:

- `cd MVP && cargo test -p mvp-bus`

### U2. Bus-wide bounded delivery execution

Files:

- Modify `MVP/bus/src/memory.rs`
- Add focused tests in `MVP/bus/src/memory.rs`

Approach:

- Move from per-dispatch worker spawning to a bus/runtime-owned bounded
  delivery executor.
- Preserve the drain invariant from slice 001: accepted deliveries must be
  counted as in-flight before `drain()` can complete.
- Expose observable runtime counters only where tests and scale metrics need
  them; do not leak executor plumbing into feature-facing bus methods.

Test scenarios:

- Concurrent publish/request calls cannot exceed configured worker concurrency.
- Drain waits for accepted queued work as well as currently executing work.
- Request timeouts remain structured when delivery backlog delays a handler.
- Handler failures still reach request callers.

Verification:

- `cd MVP && cargo test -p mvp-bus`

### U3. First Kameo bus actor boundary

Files:

- Modify `MVP/bus/Cargo.toml`
- Add or modify `MVP/bus/src/actor.rs`
- Modify `MVP/bus/src/lib.rs`
- Add tests under `MVP/bus/src/actor.rs` or adjacent module tests

Approach:

- Add a small `BusActor` or `BusRuntimeActor` that owns the bus facade and
  exposes typed actor messages for publish, request, request-many, subscribe,
  queue subscribe, and drain.
- Use bounded actor mailbox configuration.
- Keep the existing `MemoryBus` contract usable for simple tests; the actor
  layer proves the local runtime ownership boundary without forcing every
  unit test through async actor setup.

Test scenarios:

- Actor publish reaches subscribers.
- Actor request returns a reply and propagates no-responder errors.
- Actor request-many aggregates capacity replies.
- Actor drain rejects new actor messages after completion.
- Bounded mailbox behavior returns visible failure/backpressure when capacity
  is exhausted, rather than silently queuing unbounded work.

Verification:

- `cd MVP && cargo test -p mvp-bus`

### U4. Scale metrics module

Files:

- Add `MVP/e2e/src/metrics.rs`
- Modify `MVP/e2e/Cargo.toml`

Approach:

- Add a small metrics helper for latency histograms and current-process memory.
- Write plain JSON without introducing a broad serialization framework unless
  implementation proves hand-built JSON is becoming error-prone.
- Keep metric names stable and explicit so future slices can compare runs.

Test scenarios:

- Histogram helper reports count, p50, p95, p99, min, max.
- Metrics writer creates the output directory and writes valid JSON.
- Missing memory data is represented as an explicit nullable/unknown field,
  not as zero.

Verification:

- `cd MVP && cargo test -p mvp-e2e`

### U5. Scale E2E scenario

Files:

- Add `MVP/e2e/src/scale.rs`
- Modify `MVP/e2e/src/main.rs`
- Add `MVP/slice-002-bus-scale-runtime.md`

Approach:

- Implement `cargo run -p mvp-e2e -- scale`.
- Run logical-node counts at 200, 1,000, and 10,000.
- For each size:
  - subscribe all logical nodes to `gateway.changed`,
  - publish one `gateway.changed` and verify all wakeups,
  - register all logical nodes as capacity responders,
  - run `request_many node.*.capacity` and verify all replies,
  - run a missing-responder variant and verify structured incomplete response
    accounting,
  - record latency and memory metrics.
- Keep timeouts short and scenario-scoped.
- The scenario should fail loudly on count mismatch, missing metrics, or
  timeout classification regressions.

Test scenarios:

- `scale` succeeds and writes `target/mvp-e2e/bus-scale-metrics.json`.
- `all` runs both `bus-contract` and `scale`.
- Metrics include all required node sizes.
- 10,000 logical nodes produce correct fanout and request-many counts.

Verification:

- `cd MVP && cargo run -p mvp-e2e -- scale`
- `cd MVP && cargo run -p mvp-e2e -- all`

## Review Risks

- Worker-pool refactor could reopen the drain acceptance/accounting race fixed
  in slice 001.
- Kameo actor wrapping could make simple business code more ceremonial if the
  public API leaks actor message plumbing.
- Shared payload migration could make tests less readable if conversions are
  not ergonomic.
- Scale metrics could become a proxy signal if they do not assert business
  counts and failure classes.
- 10,000-node runs can become slow or flaky if tests depend on production-scale
  sleeps instead of short operation-scoped deadlines.

## Required Review Focus

Run review with subagents before commit:

- Correctness: drain with queued/in-flight work, request-many counts, queue
  behavior, timeout/no-responder separation.
- Performance: worker bounds, payload clone cost, 10,000-node resource
  envelope, metrics overhead.
- Reliability: shutdown/drain, handler failure propagation, timeout handling.
- Security/authorization: grant checks still happen before dispatch after actor
  wrapping.
- Simplicity: feature-facing API remains small and business-like.

## Verification Commands

Run before committing:

```text
cd MVP && cargo test
cd MVP && cargo run -p mvp-e2e -- bus-contract
cd MVP && cargo run -p mvp-e2e -- scale
cd MVP && cargo run -p mvp-e2e -- all
cd MVP && cargo clippy --all-targets -- -D warnings
git diff --check
git diff --name-only -- ':!MVP/**'
```

## Semantic Leverage Check

This slice should make later business logic easier in two concrete ways:

- feature code can keep using the same subject/request APIs while scale,
  payload sharing, worker bounds, and metrics stay hidden in the runtime;
- actor ownership begins without forcing deploy/machine/gateway code to manage
  actor internals directly.

Record whether this held in `MVP/slice-002-bus-scale-runtime.md` after
implementation and review.
