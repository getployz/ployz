# MVP Primitive Decisions

This is the maintainer-facing map of the “Lego pieces” used in the MVP rewrite.
It is intentionally shorter than an ADR archive: each entry should explain why
the primitive exists, what it replaces, what it costs, and what would make us
revisit it.

Update this file when a slice introduces a new primitive or materially changes
one. Slice notes should link back here instead of burying architecture rationale
inside implementation reports.

The bar for adding an entry is evidence. If a slice only raises a possible
future concern, keep that in the slice report until implementation or tests make
the decision concrete.

## NATS-Shaped Bus Semantics

Why this:
- Subjects, wildcard subscriptions, request/reply inboxes, no-responder errors,
  request-many, queue groups, drain, and grants map directly to the operations
  Ployz business logic needs.
- Business code can say `request_many(node.*.capacity)` or
  `queue_subscribe(deploy.submit, schedulers)` instead of hand-wiring transport,
  fanout, response aggregation, and auth checks each time.

What it replaces:
- Ad hoc RPC enums for every cross-node workflow.
- Treating gossip as the public programming model.
- Manual “send to all peers and collect whatever comes back” loops.

Costs:
- The bus contract is now real product surface, so subject naming, grant rules,
  and request policies need discipline.
- Wildcard matching and queue semantics need stress coverage before they become
  distributed over iroh.

Revisit if:
- The subject matcher becomes a 10k-node hot path bottleneck after real
  distributed transport is in place.
- Business logic starts needing durable stream replay, which is deliberately
  not part of this primitive.

## Kameo Actor Boundary

Why this:
- Ployz subsystems should own state behind typed messages rather than sharing
  mutable internals.
- The bus actor is the first proof that future code can talk through an
  actor-owned boundary while the internal bus remains simple and testable.
- Blocking bus operations are delegated out of the Kameo mailbox, so actor
  ownership does not turn slow request/reply work into global actor head-of-line
  blocking.

What it replaces:
- Direct subsystem access to bus internals.
- Long-lived “manager” structs that own transport, authorization, state, and
  presentation at once.

Costs:
- Actor calls are async even when the current in-memory bus is sync.
- Error mapping needs to preserve domain failures separately from actor
  availability failures.
- `BusActorHandle` is the business-facing surface. The synchronous in-memory
  bus is exported only through `mvp_bus::harness::InMemoryBus` for E2E and
  substrate tests.

Revisit if:
- Actor message handling starts doing blocking work itself. Blocking delivery
  work belongs in the bus delivery runtime or future dedicated actors, not in
  the Kameo mailbox.

## Shared Payload Bytes

Why this:
- Publish fanout and request-many are hot paths. Cloning a payload for 10,000
  subscribers should clone a handle, not allocate 10,000 byte buffers.
- `Payload` gives business code a named type without forcing every caller to
  think about the underlying byte container.

What it replaces:
- Raw `Vec<u8>` as the bus payload type.
- Hidden O(N * payload_size) clone costs during fanout.

Costs:
- Tests and edge adapters sometimes need `as_bytes()` rather than direct `Vec`
  comparison.

Revisit if:
- Payloads need zero-copy file/blob references. At that point the bus payload
  should probably become an enum over inline bytes and blob references.

## Bus-Wide Delivery Runtime

Why this:
- Scale should be bounded by a known runtime shape, not by spawning a new worker
  set per publish/request call.
- Drain must wait for queued and running delivery work, not only handlers that
  already started.

What it replaces:
- Per-dispatch worker pools.
- Accidental unbounded thread creation under load.

Costs:
- The in-memory MVP now has a queue capacity and runtime metrics that tests must
  keep honest.
- Backpressure is currently producer blocking inside the bounded delivery queue.
  The scale E2E saturation case asserts both full-queue observation and bounded
  worker concurrency; future slices should make mailbox/queue saturation
  operator-visible where foreground callers need a structured timeout or
  rejection.
- Publish, request, and request-many paths all use bounded delivery enqueue and
  response waits. Saturated enqueue attempts record pressure even when they time
  out instead of disappearing from runtime metrics.

Revisit if:
- Delivery handlers become async iroh operations. The runtime may move from
  threads to a Tokio task pool, but the “one owned bus runtime” shape should
  remain.

## hdrhistogram and memory-stats for E2E Proof

Why this:
- The MVP needs fast feedback that reports latency percentiles and memory shape
  for 200, 1,000, and 10,000 logical-node runs.
- These crates keep measurement code boring and avoid inventing percentile math.

What it replaces:
- Hand-rolled elapsed-time-only metrics.

Costs:
- These numbers are machine-local development signals, not release SLOs.

Revisit if:
- We need long-running benchmark trend storage or CI dashboards. That should be
  a harness concern, not bus business logic.
