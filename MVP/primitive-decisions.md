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

## Changed Since Last Slice

- Slice 002 replaced per-dispatch worker creation with one bus-wide bounded
  delivery runtime. The current shape is intentional and carries runtime
  pressure metrics.
- Slice 004 promoted bridge availability from hidden forwarding behavior to
  explicit enabled/disabled rule state. The test-only `RemoteUnavailable`
  variant has now been removed because no production observation pipeline set
  it; remote absence should surface through no-responder/request failure until
  a real probe path exists.
- Slice 005 made SQLite disposable and moved serving truth to deterministic
  fact reduction plus atomic local outputs.
- Fact conflicts are no longer write-time errors in the in-memory harness.
  Conflicting candidates are stored and surfaced to projection reducers, which
  matches the iroh-docs/CRDT reality better than rejecting the second writer.
- The gateway/DNS plan that preserved the old role shape was superseded. HTTP
  and DNS behavior remain product requirements; Pingora and the existing DNS
  code are references, not constraints.
- Slice 007 raised only the isolated `MVP/` workspace to Rust 1.91 and added
  the current iroh family behind a new `mvp-iroh` crate. Root workspace
  toolchain policy remains unchanged.
- Slice 007 added a docs-backed `FactSource` adapter with a synchronous local
  view. Projection reducers still do not depend on raw iroh types or async docs
  APIs.
- Slice 007 kept the original session-based fact authorizer method and added a
  principal-based check for docs authors. That preserves existing call sites
  while allowing imported iroh-docs entries to be authorized without fabricating
  a bus session.
- Slice 007 made payload availability explicit in the docs local view: metadata
  for a newer same-author fact replaces older payload-bearing metadata, even if
  the new blob content is not ready yet. Reducers see the candidate but cannot
  read a stale payload.
- Slice 007 made malformed docs entries visible as rejected entries while valid
  facts in the same refresh continue to apply. A bad non-Ployz docs key should
  not freeze the local projection view.

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
- The latest local scale run showed bridged 10,000-subscriber stream p99 around
  2x plain publish p99. The current diagnosis is that a bridged stream does
  transform/match work and then dispatches a second local message with bridge
  origin metadata through the same delivery runtime. Do not add more bridge
  surface before profiling or indexing this path.
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
  the same hash, and conflicting hashes for the same fact key are stored as
  conflicting candidates for projection.
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
- Payload bytes are now stored in the harness and hashed with BLAKE3, but the
  store is still local memory. iroh-blobs still needs to supply real
  content-addressed transfer and persistence.
- Point reads of a conflicted key return no single fact. Reducers and tools that
  need truth must list candidates and handle conflicts explicitly.

Revisit if:
- Business logic starts choosing between in-memory facts and docs facts itself.
  The in-memory store is a harness; `mvp-iroh` is the first real backend behind
  the same fact-source contract.

## Singleton / Lease Primitive Gap

Why this is not decided yet:
- ACME needs exactly one issuer per challenge at a time across the cluster.
  Existing bus semantics through Slice 004 cannot express that safely.
- This primitive has partition and recovery semantics, so it must be selected
  deliberately before the ACME slice starts.

Options to plan with the operator:
- queue group with `max_members = 1` enforced by the bus,
- explicit lease fact with TTL and renewal as a fact-store primitive,
- named singleton service registered through `$SYS.service.*`.

Revisit:
- Immediately before ACME planning. Do not implement ACME until this choice is
  made and its failure semantics are captured in the slice plan.

## Iroh Toolchain And Docs Adapter

Why this:
- The MVP strategy depends on iroh, iroh-gossip, iroh-blobs, and iroh-docs as
  deployed substrate, not only as future references.
- Several completed slices intentionally proved semantics in memory first. That
  phase is no longer enough; future transport/fact slices must bind the
  semantics to real iroh APIs.
- The projection path must stay synchronous from the reducer's point of view:
  async docs sync updates a local view, and projection reads that local view
  through `FactSource`.

Decision:
- `MVP/` now declares Rust 1.91 so it can use current iroh crates:
  `iroh 1.0.0-rc.0`, `iroh-docs 0.99.0`, `iroh-blobs 0.101.0`, and
  `iroh-gossip 0.99.0`.
- The root workspace is not changed by this decision.
- Raw endpoint/docs/blob/gossip types are confined to `mvp-iroh` internals and
  E2E harness setup. Business reducers, deploy logic, and bus semantics should
  consume typed Ployz contracts.
- Docs author IDs map through explicit Ployz principal bindings. Unknown docs
  authors are unverified; docs access is not authority.
- Malformed docs entries are reported through the `mvp-iroh` local-view
  rejected-entry surface and skipped. Valid facts in the same docs query still
  flow into projection.
- Author bindings are currently explicit test/bootstrap data. Before production
  join or long-lived docs access, this needs a replicated author-binding
  manifest or equivalent membership fact so newly authorized docs authors become
  verifiable on already-imported peers.

Revisit:
- If Rust 1.91 becomes unacceptable even inside `MVP/`, pinning an older iroh
  line must be planned as its own compatibility slice.
- If projection ever needs to await iroh APIs directly, redesign the adapter
  boundary instead of letting async transport concerns leak into reducers.

## Coordinator Is Not Steady State

Why this:
- The daemon should coordinate mutations, not be the reason running services,
  WireGuard, HTTP serving, DNS serving, fact-sync, projection, or snapshot
  application stay alive.
- The strongest MVP failure proof is killing the command/coordinator role and
  seeing steady state continue from last applied local state while new
  replicated serving-state facts still reduce into gateway/DNS snapshots.

What it replaces:
- A single daemon process that owns command routing, data-plane serving,
  projection application, runtime mutation, and liveness as one fate-sharing
  unit.
- Treating "daemon down" as "node dead." The node may be unable to accept new
  mutations, but existing workloads and data-plane paths should remain useful.

Costs:
- Future process-role design has to distinguish coordinator, runtime/applier,
  HTTP/DNS serving, fact-sync/projection, and transport responsibilities.
- Health surfaces must be precise: coordinator stale/unavailable for mutations
  is not the same as workload, WireGuard, or serving failure.

Revisit if:
- A future slice proves that a responsibility cannot safely run outside the
  coordinator. That slice must document the failure audience and the exact
  data-plane behavior lost when the coordinator dies.

## Deterministic Projections And Atomic Snapshots

Why this:
- Durable facts should reduce into the serving view without making SQLite
  authoritative. The projection reducer is pure, deterministic, and can rebuild
  `projections.sqlite`, `gateway.snapshot`, and `dns.snapshot` from facts.
- Gateway and DNS process roles need complete local snapshot files they can keep
  serving through daemon restarts. Snapshot replacement must be atomic so a
  failed write leaves the last good file intact; gateway and DNS snapshots are
  written as one projection batch with rollback on partial failure.
- `ProjectionActor` gives the pipeline an actor-owned boundary: one island, one
  read-authorized projection principal, bounded `project_once` calls, and
  structured last-success/last-failure status.
- Fact payload reads are authorized through the fact identity, not a global hash
  lookup. A principal may know a content hash without gaining access to payload
  bytes written under another island/key.
- The reducer validates that payload identity matches the fact key identity
  before projecting. Grants authorize keys, so payloads cannot smuggle a
  different node, service, route, HTTP, or DNS commit into the serving view.
- SQLite is staged before snapshot publication and promoted only after the
  serving-state snapshot batch succeeds. A failed snapshot write therefore leaves
  readers on the last successful SQLite projection and last successful
  snapshots.

What it replaces:
- Mutable SQL head rows as the source of truth.
- Manual event-log gap repair before rebuilding HTTP/DNS serving state.
- Treating fact notifications as correctness-critical delivery.

Costs:
- Full rebuild is intentionally the correctness path. The current 10,000-node
  local scale run is fast enough, but future docs-backed replication still needs
  propagation-lag proof.
- Snapshot schema is MVP-local JSON. Existing Pingora gateway and DNS binaries
  do not consume it, and future serving slices may redesign the state shape.
- The in-memory fact source can mark envelopes as verified only by local harness
  rules. A real iroh-docs adapter must validate namespace/author signatures
  before returning `Verified` candidates.

Crates:
- `rusqlite` keeps the rebuildable SQLite projection store direct and small.
- `tempfile` handles same-directory temp files followed by atomic persist.
- `blake3` gives fact payloads content-addressed hashes aligned with the iroh
  ecosystem.
- `thiserror` keeps projection errors structured without hand-written boilerplate.

Revisit if:
- The iroh-docs/toolchain slice chooses to raise `MVP/` to Rust 1.91 for current
  `iroh-docs`, or pins an older compatible iroh-docs line. That slice should
  implement the adapter behind the fact-source contract, not rewrite reducers.
- HTTP/DNS serving needs a binary or version-negotiated state format after the
  MVP-local JSON schema has proven too slow or too rigid.

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
