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

## Authority Islands and Grants

Why this:
- Subjects and fact keys are island-local. A laptop and prod can both use
  `deploy.submit` or `/facts/deploy/d1/plan` without sharing authority or
  truth.
- `BusSession` carries `IslandId` plus `PrincipalId`, so business code cannot
  accidentally choose a remote authority island by writing a longer subject
  string.
- Grants authorize publish, subscribe, queue subscribe, response, drain,
  fact-read, and fact-write operations before handlers or durable mutation run.

What it replaces:
- A single global bus namespace.
- Treating transport identity, such as a future iroh endpoint key, as authority.
- Product logic that manually checks "is this prod?" before every operation.

Costs:
- Every dispatch now filters by island as well as subject pattern.
- Revocation is stateful. In this slice it is in-memory; future replicated
  revocation facts need the same explicit operator-visible shape.
- Import/export bridges must be explicit. There is deliberately no hidden
  cross-island forwarding path yet.

Revisit if:
- Operator-editable policies need a real policy language. `cedar-policy` is the
  likely candidate if grants outgrow simple product-owned allow/deny lists.
- Delegated invite or bridge tokens need offline attenuation. `biscuit-auth`
  should be reconsidered then, with revocation still modeled as cluster state.

## Authority Bridges

Why this:
- Authority islands should stay isolated by default, but Ployz still needs
  deliberate laptop-to-prod and dev-to-prod workflows. A bridge is the explicit
  import/export rule that says which service request or message stream may
  cross islands.
- Service imports keep remote mutation foreground: a laptop request to
  `gpu.deploy.submit` maps to prod `deploy.submit`, receives one reply, and
  fails visibly if the bridge is disabled.
- Stream imports are one-way visibility, not shared truth. Imported messages
  carry bridge-origin metadata with source island, source principal, original
  subject, and rule id.
- Both sides use grants. The remote bridge principal must be allowed to publish
  imported service requests or export a stream, and the local bridge principal
  must be allowed to publish the mapped imported stream.
- `ServiceImport` uses named `BridgeEndpoint` values for local and remote
  endpoints so bridge setup code cannot swap authority boundaries through a
  long positional constructor.

What it replaces:
- Prefixing every subject with island names.
- Hidden global forwarding when no local responder exists.
- Letting a laptop principal mutate prod facts directly.
- Treating service discovery or transport as the authority decision.

Costs:
- Bridge rules are now another authority surface and need collision checks.
  Duplicate rule IDs, duplicate service imports, local responder/import
  conflicts, and ambiguous stream-source mappings are rejected instead of
  relying on precedence.
- Imported stream delivery adds work to publish paths that match bridge rules.
  The scale harness now measures 200, 1,000, and 10,000 imported stream
  subscribers plus a 10,000-rule matching stream fanout.
- The current bridge is an in-memory contract harness. Future slices still need
  docs-backed rule replication and iroh transport.

Revisit if:
- Bridge rule reads become a hot path under distributed transport. `arc-swap`
  is the likely fit for immutable rule snapshots.
- Bridge tasks need cancellation and outage propagation across async iroh
  workers. `tokio-util::sync::CancellationToken` should be reconsidered then.
- Delegated bridge/invite credentials need offline attenuation. Re-evaluate
  `biscuit-auth`, with revocation still represented as cluster state.

## Immutable Fact-Set Harness

Why this:
- The architecture needs signed, mostly immutable facts projected into SQLite,
  not a manual SQLite event log with gap repair.
- This slice proves the business contract before iroh-docs arrives: fact reads
  and writes are island-scoped, authorized by grants, writes are idempotent for
  the same hash, and writes return a structured conflict for a different hash.
- `BusActorHandle` exposes fact writes/reads so future business logic uses the
  actor boundary instead of inspecting grants or storage internals.

What it replaces:
- Letting early code write mutable "head" rows as authority.
- Treating SQLite as cluster truth.
- Testing authorization only through bus messages while durable fact mutation
  remains unchecked.

Costs:
- The current store is an in-memory contract harness, not durable replication.
- Read authorization is intentionally present even though facts are local for
  now. A revoked session or an empty grant cannot read facts in its island.
- `FactContentHash` is only a typed hash string until iroh-blobs supplies real
  content-addressed payloads.

Revisit if:
- Slice work reaches iroh-docs integration. At that point this harness should
  become a backend behind the same fact contract, not a parallel source of
  truth.

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
