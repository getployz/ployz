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
- Direction after Slice 008: every node is equal and the operator's connected
  node is the consistency boundary. Fact writes commit durably to local docs
  and replicate eventually; no `min_replicas`, commit quorum, pin-fact
  acknowledgement collection, or lease witness collection.
- Direction after Slice 008: leases are advisory facts for TTL, renewal, epoch
  fencing, RAII release, and command-level race avoidance. Resource-level
  enforcement owns real exclusivity.
- Slice 009 implemented the first advisory lease/ACME canary in memory:
  deterministic supersession, local visible-node command context, stale-holder
  fencing, book-bound guards, claim-hash-fenced renew/release facts, and
  best-effort RAII release. Stale imported renewals are reduced
  chronologically, so an after-expiry renewal candidate cannot resurrect an
  expired lease. The primitive is intentionally not docs-backed yet.
- Slice 009 keeps future active-member or partition-view checks as explicit
  command evidence only. They may improve "visible nodes at decision time"
  later; they are not part of the commit boundary and must not become hidden
  quorum behavior.
- Slice 010 adds the first deploy commit-before-drain canary. Aggregate
  `ServingCommit` facts are the local durable route/gateway/DNS fact boundary
  and the drain gate; projection catch-up is evidence before old-instance
  stop, not authority. This replaces the earlier three-head write shape for
  deploy cutover.
- Slice 010 makes cleanup require a typed `ProjectionCatchUp` proof derived
  from local projection/snapshot output. The proof matches IDs, snapshot
  revisions, gateway routes, active backends, old backends, and DNS records. A
  raw pending cleanup value is not enough to drain or stop old backends.
- Slice 010 changes deploy admission from open wildcard capacity trust to exact
  planned-node capacity probes. Wildcard capacity remains observation; planned
  mutation admission must bind the response to the requested node and validate
  the capacity fields it requested.
- Slice 010 separates phase reversibility from serving publication. "Can this
  phase be rolled back?" and "does this phase publish serving state?" are
  different deploy dimensions.
- Slice 010 makes serving/gateway/DNS commit candidates reduce by
  `(epoch desc, content_hash asc)` with every non-winning candidate counted as
  `Superseded` instead of falling back to generic conflict/no projection.
- Slice 010 adds a harness-only way to pair a `BusActorHandle` with the same
  in-memory bus used by `BusFactSource`. Feature code still uses actor handles;
  raw `MemoryBus` remains a projection/E2E fixture.
- Slice 011 adds actor-owned last-good serving state. Serving loads validated
  gateway/DNS snapshot batches, answers typed gateway/DNS queries from memory,
  preserves last good on unsafe reload, and surfaces freshness/reload failure
  status.
- Slice 011 proves coordinator-down serving semantics with
  `steady-state-serving-contract`: the local coordinator is absent for
  mutations, a separate harness serving-commit writer still projects locally,
  a fresh projection actor rebuilds deleted SQLite from facts, and serving
  keeps answering while rebuild is in flight.
- Slice 011 keeps active-member/partition-view checks deferred. Future slices
  may add them as explicit command evidence for "visible nodes at decision
  time"; they are not a hidden quorum or serving commit gate.
- Slice 012 moves coordinator-down serving from an in-process semantic proof to
  real OS process roles inside `mvp-e2e`. The local coordinator can be killed,
  typed gateway/DNS queries keep answering from the serving/projection process,
  and later already-authorized facts arrive through a remote-replication
  injector rather than through a revived local coordinator.
- Slice 012 adds a harness-only `process_fact_source` and PID cleanup registry
  for process-role E2E. This is not a production fact backend or supervisor; it
  exists to prove fate separation and to keep `mvp-e2e -- all` cleanup bounded
  on timeout.
- Slice 013 adds real wire HTTP/DNS serving roles inside `MVP/`. The HTTP proof
  uses Hyper instead of Pingora but still proxies through a deterministic
  backend; the DNS proof uses `hickory-proto` instead of `hickory-server` but
  still parses/encodes real DNS packets.
- Slice 013 moves wire request lookups off the serving actor mailbox. The actor
  remains the reload/status owner, while HTTP/DNS hot-path reads use a shared
  last-good snapshot holder so concurrent serving traffic is not serialized
  through the control-plane actor.
- Slice 013 hardens the process-role PID registry: records include child
  executable identity, cleanup is best-effort across all records, stale/raced
  child exits do not stop the sweep, and dropping a running child leaves its PID
  file for timeout cleanup.
- Slice 014 adds MVP-local membership and full-mesh WireGuard planning. Join
  facts carry iroh endpoint and WireGuard identity; tombstone facts dominate
  normal future joins/services until an explicit reinvite/clear primitive exists.
- Slice 014 splits join writers from tombstone writers at the fact-grant layer.
  A principal that can write `/facts/node/*/joined/>` cannot remove machines by
  writing `/facts/node/*/tombstoned/>`.
- Slice 014 proves a last-applied mesh data-plane process role. Outbound
  service traffic resolves target sockets from the applied peer snapshot, not
  from caller-supplied addresses, and continues while the local coordinator is
  killed.
- Slice 014 keeps active-member/partition-view checks deferred. The membership
  proof reports visible nodes and tombstone facts, but it does not introduce
  quorum, witness acknowledgements, or hidden active-partition commit rules.
- Slice 015 docs-backed ACME projection now treats HTTP-01 presentation facts
  as valid only when the docs author, lease holder, epoch, and claim hash match
  the current advisory lease candidate. HTTP-01 projections carry the active
  lease expiry, and serving refuses expired challenges at request time. ACME
  hostnames and tokens are revalidated when facts deserialize, and key
  authorization is revalidated when presentation facts and serving snapshots
  load, so wire serving can answer from validated last-good state without
  reparsing on each request.
- Slice 016 introduces a tiny `mvp-identity` crate for shared MVP identities.
  `NodeId` and `VisibleNodes` are one real type across lease, ACME, projection,
  deploy, mesh, and E2E code. This avoids a projection/lease dependency cycle
  while removing `DeployNodeId` and the lease/deploy/mesh visible-node wrappers.
- Slice 016 also makes WireGuard peer snapshots carry typed routing fields:
  `WireGuardOverlayCidr` for allowed IPs and `IrohEndpointId` for peer
  endpoints. `WireGuardOverlayCidr` is intentionally only a typed `/128` host
  route for now; `ipnet` was reviewed and deferred because arbitrary CIDRs are
  not a current MVP behavior.
- Slice 017 adds graceful machine remove as a product command over existing
  primitives. It writes docs-backed `NodeRemovalStarted`/`NodeTombstoned`
  facts through a `MachineFactWriter`, uses the shared `ServingCommit` cutover
  primitive for route removal, gates final stop on `ProjectionCatchUp`, and
  leaves real runtime/container stop implementations behind the participant
  ABI.
- Slice 017 also adds a harness-local combined `FactSource` for the E2E proof
  because membership/removal facts are docs-backed while serving commits still
  come from the bus-backed routing primitive. This is a proof bridge, not a new
  production substrate; Slice 018 is planned to move deploy recovery proof to
  one docs-backed fact source/sink for command and serving facts.

## Documented Design Gaps

These are known gaps, not hidden behavior. Do not solve them until a slice has
the metric or product proof that makes the extra primitive worthwhile.

- Deploy pre-commit participant cleanup still needs an explicit ABI. If
  `prepare_instance` succeeds for one participant and a later
  `start_instance` fails before any durable serving commit, the coordinator
  currently relies on participant idempotency and later explicit cleanup rather
  than substrate-driven compensation. The next deploy slice that expands
  participants must name the contract for prepare-without-start and
  start-without-commit cleanup.
- Lease reduction walks accumulated facts repeatedly per resource. That is fine
  for the current proof but will become expensive once docs-backed leases carry
  months of renewal/release facts. Add compaction only after real iroh-docs
  lease replication ships and there is a renewal-volume metric to drive the
  threshold.
- ACME projection currently has a small read-only lease resolver to bind
  presentation facts to active advisory leases. Before another projection
  consumer needs lease state, extract a reusable read-only reducer from
  `mvp-lease` so projection does not grow a parallel lease implementation.
- `mvp-iroh` fact waits currently poll with a short sleep and full key refresh.
  This was acceptable for the slice-007 proof, but live iroh-docs events should
  replace polling once the production docs subscription path is wired.
- Projection `project_once` checks its deadline before each publish stage, but
  does not report a timeout while a blocking SQLite/filesystem publish is
  already in flight. Returning failure while a detached mutating worker keeps
  publishing would be worse than a late reply. If a hard wall-clock cap becomes
  required, the publish path needs cooperative cancellation instead of detached
  worker timeouts.

## Placeholder Versus Production Code

Reviews should separate placeholder wire proof code from interfaces intended to
survive migration.

- `MVP/serving/src/http_gateway.rs` proves HTTP wire behavior with Hyper.
  Production connection limits, keepalive behavior, proxy framing, and the
  likely Pingora implementation belong to a later migration slice.
- `MVP/serving/src/dns_server.rs` proves DNS packet behavior with
  `hickory-proto`. Full DNS server integration belongs with the production
  serving migration.
- `MemoryWireGuardBackend` and the in-memory `LeaseBook` are fixtures/proofs.
  The durable interfaces around snapshots, appliers, lease facts, status, and
  validated reload semantics are the code review should treat as lasting.

If a finding is about code that will be deleted in the next migration slice,
record it only if it threatens the proof. If it is about a surviving interface,
treat it normally.

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

## Command Consistency Boundary

Why this:
- Ployz is a small-cluster operator tool, not a consensus database. The
  operator is connected to a node, and that node is the consistency boundary for
  the command being run.
- Waiting for quorum-style acknowledgement would make reachability a hidden
  policy. Operators need to see reachability, not have it silently converted
  into blocked progress.

Decision:
- A foreground command reads relevant durable facts on its connected node before
  its first mutation.
- If preconditions already conflict, the command fails with a structured
  `Conflict` variant that names the conflicting fact, principal, and time.
- If the command proceeds, it writes intent/lifecycle facts durably to local
  docs and returns.
- Every command result includes visible nodes at decision time.
- Replication to other nodes is eventual through iroh-docs. There is no
  `min_replicas` knob, no `store.pin_fact` commit path, and no witness-ack
  collection.

What it replaces:
- Durability quorum language that made `store.pin_fact` look like a required
  commit phase.
- Hidden dependence on live-node counts to decide whether an operator command
  may finish.

Costs:
- A command can return before other nodes have observed its fact. Projection and
  serving-state tests must prove last-good behavior during propagation lag.
- If a race survives into replication, the reducer must make the outcome and
  superseded loser visible instead of pretending the race did not happen.

Revisit if:
- Product requirements explicitly require consensus for one operation. That
  should be a new primitive with a named failure audience, not a hidden mode on
  facts, deploy, or leases.
- A future membership or active-partition view proves useful enough to check
  known-alive members before mutation. That should enrich command preconditions
  and visible-node reporting, not become a hidden peer-ack commit requirement.

## Conflict Candidates And Supersession

Why this:
- iroh-docs is a replicated set. Conflicts are possible facts, not transport
  exceptions.
- Operators should not be asked to pick a winner interactively for every
  surviving race. The command surface should fail before mutation when it sees
  the conflict, and reducers should handle later races deterministically.

Decision:
- Fact storage keeps conflicting candidates.
- Command entry fails loudly on already-visible conflicts before mutation.
- Reducers order surviving candidates by `(epoch desc, content_hash asc)`.
- The chosen candidate becomes projected state.
- Losers are annotated in projection status as
  `Superseded { by_epoch, by_principal, at }`.
- Operator status surfaces `Superseded` events for the operator's own commands.

What it replaces:
- Write-time conflict rejection as the fact-store contract.
- "Operator picks" conflict resolution.
- Reducers silently ignoring a conflicting entry with no audience.

Costs:
- Reducers must be explicit about the epoch they compare. If a fact kind has no
  epoch, the slice that introduces it must define its deterministic ordering.
- Status surfaces need to retain enough provenance to tell an operator which of
  their commands was superseded and by whom.

Revisit if:
- A fact kind cannot define deterministic supersession without semantic loss.
  That fact kind probably needs a different command primitive.

## Advisory Lease Facts

Why this:
- ACME needs one issuer to act on a challenge at a time in the normal case.
  Existing bus semantics cannot express even advisory ownership safely by
  themselves.
- The same primitive will likely apply to deploy ownership, subnet claims,
  machine removal coordination, and future single-writer operations.
- Ownership loss needs to be branchable business state, not a generic transport
  error or hidden queue behavior.

Decision:
- Use explicit lease facts with resource, holder, epoch, TTL, expiry, renewal,
  release, RAII release-on-drop for local holders, and fencing token.
- Treat leases as advisory. They help command entry detect conflicts and carry
  epoch fencing into resource-specific code.
- Use the existing fact conflict contract. Conflicting claims remain candidates
  for the reducer and are ordered deterministically, not rejected by a special
  lease quorum path.
- Require product mutations that depend on advisory ownership to carry the
  current lease epoch/fencing token and re-check local lease state immediately
  before mutation.
- Leave real exclusivity to the resource-level enforcement point: ACME
  directory/challenge validation, storage backend, filesystem primitive, or
  equivalent.

What it replaces:
- NATS/JetStream-style locks for ACME issuance.
- Queue-group singleton semantics that hide failure and partition behavior in
  bus dispatch.
- Named singleton service registration as an authority mechanism.

Costs:
- A lease is not a linearizable lock on top of iroh-docs.
- A holder may lose in projection after a surviving race. The operator status
  surface must show that supersession loudly for commands they initiated.
- Resource adapters still need their own fencing or conflict behavior. The
  lease does not make an unsafe backend safe.

Revisit:
- If a future product operation truly needs exclusive mutation under partition,
  add a resource-specific enforcement primitive. Do not add a hidden "strict
  lease" mode.

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
- Process-role E2E needs explicit child cleanup. Slice 012 uses PID files only
  in the harness so a global `all` timeout can best-effort kill roles it
  abandoned; this is not a production service manager.

Revisit if:
- A future slice proves that a responsibility cannot safely run outside the
  coordinator. That slice must document the failure audience and the exact
  data-plane behavior lost when the coordinator dies.
- We need production role supervision, restart policy, or cross-platform local
  sockets. That should be a serving/runtime slice, not an expansion of the E2E
  harness.

## Process-Role E2E Harness

Why this:
- In-process actor tests cannot prove fate separation. The MVP needs real child
  processes so killing the local mutation role does not accidentally kill the
  serving/projection role.
- The process harness lets us stress "daemon down, steady state still works"
  before committing to the final Pingora/Hickory/WireGuard process layout.

What it replaces:
- Treating one daemon binary/process as the unit of all behavior.
- Tests that only set a boolean "coordinator unavailable" while all actors live
  in the same process.

Costs:
- The harness has more plumbing than the business invariant. Unix sockets,
  child waits, PID files, timeout cleanup, and one-shot role dispatch are all
  test substrate.
- `process_fact_source` is deliberately file-backed and local. It proves OS
  process fate separation; it does not replace iroh-docs replication.
- The PID registry now records child executable identity and removes records
  only after observed exit or best-effort timeout cleanup. It is still a test
  cleanup mechanism, not production supervision.

Crates:
- `tokio::process` owns child lifecycle.
- `tokio::net::UnixListener` / `UnixStream` own harness-local IPC.
- `serde_json` keeps requests/responses typed without a general IPC framework.
- `interprocess`, `assert_cmd`, `clap`, `tokio-util`, and process supervisor
  crates were reviewed and deferred.

Revisit if:
- The role IPC becomes user-facing or cross-platform. Use `clap` for real CLI
  parsing and reconsider `interprocess` for local socket portability.
- Process lifecycle becomes product behavior. Add a real supervisor boundary
  instead of growing the E2E PID registry.

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

## Actor-Owned Last-Good Serving State

Why this:
- Gateway and DNS serving should not depend on a live coordinator, SQLite hot
  reads, bus requests, or fact projection during steady-state queries.
- The serving role needs one owner for loaded snapshot revisions, reload
  attempts, freshness, and last structured reload failure.
- Loading gateway and DNS snapshots as one validated batch keeps HTTP-facing
  and DNS-facing views from diverging after a partial or unsafe reload.

What it replaces:
- Feeding serving roles through the old NATS-store synchronization/input model.
- Treating daemon death as serving death.
- Re-reading SQLite or durable facts on each gateway/DNS request.

Costs:
- Slice 011's original proof was typed query semantics. Slice 013 now adds real
  wire traffic, but Pingora and `hickory-server` integration remain separate
  proof gaps.
- The actor clones route/record answers for callers. Wire handlers no longer
  pay actor-mailbox serialization for each request, but the snapshot holder is
  still a simple `RwLock`; future traffic tests can justify lock-free immutable
  snapshot pointers.
- Snapshot reload is explicit. Automatic file watching is deferred until the
  replacement contract is boring.

Crates:
- `kameo` owns the serving actor mailbox and typed messages.
- `mvp-projection` owns snapshot schema and validation.
- `notify`, `pingora`, `hickory-server`, `axum`, and `arc-swap` were reviewed
  for the semantic slice and deferred to the first wire/process slice that needs
  them.

Revisit if:
- Wire traffic benchmarks show `RwLock` contention. `arc-swap` is the likely fit
  for immutable last-good snapshot pointers.
- File watcher reload becomes product behavior. Add `notify` only after
  explicit reload semantics remain tested.
- Gateway or DNS state shape needs binary/versioned compatibility instead of
  MVP-local JSON.

## Wire HTTP/DNS Serving Roles

Why this:
- Typed gateway/DNS queries were not enough proof. The MVP needs real HTTP and
  DNS sockets that keep serving while the local mutation coordinator is dead.
- HTTP/DNS roles should consume last-good serving snapshots only. They must not
  read SQLite, facts, bus state, or coordinator state on request paths.
- The same shipped artifact can run different roles, but the fate boundary is
  explicit: killing the coordinator does not kill HTTP/DNS serving.

What it replaces:
- The old assumption that preserving a Pingora gateway input model or DNS
  process shape is a non-negotiable migration constraint.
- A single process where command coordination, projection, and serving all share
  one failure fate.
- Per-request control-plane actor calls in the wire hot path.

Costs:
- The HTTP implementation uses Hyper rather than Pingora. It proves real
  backend traversal and request routing, but not Pingora-specific lifecycle or
  proxy APIs.
- The DNS implementation uses `hickory-proto` directly over Tokio UDP rather
  than `hickory-server`. It proves real DNS packet parsing/encoding, but not
  Hickory server handler integration or TCP fallback.
- The E2E harness now owns more process plumbing. That plumbing is proof
  substrate, not product supervision.

Crates:
- `hyper`, `hyper-util`, and `http-body-util` own the HTTP/1 server proof.
- `hickory-proto = 0.26.1` owns DNS message parsing/encoding on the patched
  Hickory protocol line.
- `tokio::net` owns loopback TCP/UDP listeners and Unix control sockets.

Revisit if:
- Version 1 needs Pingora-specific behavior such as production proxy lifecycle,
  upstream policy, TLS, or richer gateway features. Add Pingora against the new
  snapshot input model, not the old daemon/store coupling.
- DNS needs authoritative-zone abstractions, TCP fallback, or richer record
  handling. Integrate `hickory-server` behind the same last-good snapshot
  boundary.
- Wire request throughput exposes snapshot-read lock contention. Move from
  `RwLock` to immutable `Arc` snapshots swapped by reload.

## Membership And Last-Applied WireGuard

Why this:
- Machine add/remove is one of the product's core primitives. The foundation
  needs typed membership facts and peer planning before deploy/runtime slices can
  rely on the private data plane.
- WireGuard is steady-state data plane. The coordinator may request changes,
  but service-to-service traffic must keep using the last applied peer config
  while the coordinator is down.
- Tombstones are explicit operator intent. They should not be undone by a later
  normal join fact that races in through eventual replication.

Decision:
- `NodeJoined` facts carry node id, epoch, derived overlay IP, iroh endpoint
  identity, and WireGuard public key.
- `NodeTombstoned` facts remove the node from live membership and service
  projections. They dominate later normal join/service facts until a future
  explicit reinvite/clear primitive exists.
- Join and tombstone mutation authority use separate fact grants.
- Node identity is shared through `mvp-identity::NodeId`. This crate exists
  because `mvp_projection` depends on lease and ACME fact payloads, so
  lower-level lease/ACME code cannot depend on projection without a cycle.
- For `<= 32` live nodes, the MVP plans full-mesh peers from projection state.
  Malformed remote peer identities are skipped; malformed or missing local
  identity fails planning.
- The E2E data-plane role loads a last-applied snapshot before serving and
  resolves outbound targets from that snapshot. A caller may name a peer, but
  cannot supply an arbitrary socket address.
- Applied WireGuard peer snapshots carry `WireGuardOverlayCidr` instead of raw
  allowed-IP strings and `IrohEndpointId` instead of raw endpoint strings.

What it replaces:
- Treating membership as implied liveness or freshness.
- A manual IPAM/lease table for overlay addresses.
- A coordinator-owned data plane that disappears when the daemon dies.

Costs:
- The current proof uses loopback TCP gated by the WireGuard peer snapshot. It
  proves membership, applied-config, and process fate semantics, not encrypted
  kernel WireGuard packets.
- Graceful remove is proven through a fixture-backed participant contract:
  target probe, no-new-work-and-drained acknowledgement, serving cutover,
  projection catch-up, stop, tombstone, projection rebuild, and peer-plan
  exclusion. Real runtime/container stop and transfer backends are still
  deferred.
- Reinvite is intentionally absent. The system rejects normal rejoin for a
  tombstoned node id until a later slice defines that primitive.

Crates:
- `kameo` owns the WireGuard actor boundary and bounded apply/status messages.
- `tempfile` owns random same-directory snapshot temp files before atomic
  persist.
- `defguard_wireguard_rs`, `wireguard-control`, and `boringtun` were reviewed
  and deferred. `defguard_wireguard_rs` remains the likely production host
  adapter candidate once real interface mutation enters scope.
- `ipnet` was reviewed for CIDR representation and deferred. The MVP currently
  derives only `/128` host routes from overlay IPs, so a tiny typed wrapper is
  simpler than a generic network-prefix dependency.

Revisit if:
- Product requirements need partial WireGuard graph selection beyond full mesh.
- A future active-member or partition-view primitive becomes useful as explicit
  command evidence. It must enrich visible-node reporting and precondition
  failures, not become a hidden quorum/peer-ack commit rule.
- Production WireGuard adapter work starts. Add it behind the existing
  `WireGuardBackend` boundary rather than changing membership reducers or join
  command semantics.

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
