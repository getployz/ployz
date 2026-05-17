---
title: MVP E2E Proof Plan
status: active
created: 2026-05-17
---

# MVP E2E Proof Plan

The MVP is only credible if end-to-end tests prove the architecture under load
and failure. Unit tests can shape individual reducers and actors, but the main
deliverable is a repeatable E2E harness that exercises real message paths,
fact replication, projection rebuilds, HTTP/DNS continuity, WireGuard
reconciliation, and deploy crash points.

The proof also has to include code-shape evidence. The previous foundation had
too much substrate code for too little business logic. The MVP should
reimplement representative product behavior and show that the new primitives
make business rules easier to express and test.

The first proof should be narrow enough to guide implementation:

```text
Two nodes can join over the new substrate, exchange bus requests, replicate one
route fact, rebuild a local projection, publish serving state, and keep
HTTP/DNS serving last-good state through daemon restart.
```

That proof is the first vertical slice. It is not the whole MVP. The whole MVP
must also stress the new foundation hard enough to make the version-1 decision
with confidence.

## Test Harness Shape

Build the MVP proof harness under `MVP/`. Existing harnesses such as
[crates/ployz-e2e](../crates/ployz-e2e) and
[crates/ployz-sim](../crates/ployz-sim) are reference material for patterns,
but the rewrite must remain self-contained in `MVP/` until migration is an
explicit decision.

Start each slice with the smallest harness extension that proves that slice.
The full MVP still includes large logical-node stress tests.

Likely harness stages:

| Stage | Purpose |
| --- | --- |
| In-process semantic harness | Deterministic bus/fact/projection behavior. |
| Two-node local process harness | Real daemon/serving-role boundaries for restart proof. |
| Logical-node stress harness | Required large-load proof once bus/fact behavior exists. |

The product target remains 1-200 real nodes from [VISION.md](../VISION.md).
For the MVP, real process/WireGuard proof can stay smaller while logical
bus/fact/projection tests must exercise 200, 1,000, and 10,000-node loads. Those
larger tests prove margin in the control-plane design; they do not imply a
10,000-node WireGuard operational topology.

## Test Principles

- Every mutating operation has a foreground audience and structured failure.
- No test should pass because a background loop eventually rewrote durable
  truth.
- Timeouts are part of the behavior. No external control-plane operation may
  await indefinitely.
- Tests should verify final facts, local projections, snapshots, and live
  service behavior where relevant.
- Projection state must be disposable. Deleting SQLite should never erase
  cluster truth.
- HTTP/DNS serving must keep serving last good state while daemon/projection is
  down.
- Killing the command/coordinator role must not kill steady-state data-plane
  behavior. Existing workloads, WireGuard service-to-service traffic, and
  HTTP/DNS serving must continue from last applied state. New mutations should
  fail visibly until the coordinator is back.
- Feature tests should read as business behavior. If a test mostly scripts
  transport, storage, retries, or process wiring, the primitive below it is
  probably still too weak.

## Acceptance Suites

### E2E-1: Bus Semantics

Purpose: prove PloyzBus is NATS-shaped enough to replace Core NATS semantics.

Scenarios:

1. `publish` fans out to all matching wildcard subscribers.
2. `request` returns one response through an ephemeral inbox.
3. `request` returns `NoResponders` when no known responder exists.
4. `request` times out with a typed timeout when responders are known but do
   not answer before deadline.
5. `request_many node.*.capacity` aggregates all live responders and reports
   missing responders separately from negative replies.
6. `queue_subscribe deploy.submit queue=schedulers` delivers each request to
   exactly one scheduler.
7. Queue drain stops new deliveries and lets in-flight requests finish or time
   out visibly.
8. Unauthorized publish, subscribe, queue subscribe, and response attempts fail
   before handler dispatch.

Metrics:

- publish p50/p95/p99
- request p50/p95/p99
- request-many aggregation duration
- no-responder detection time
- queue delivery skew

### E2E-2: Authority Islands

Purpose: prove single-island authority checks before adding bridge complexity.

Scenarios:

1. A principal with publish permission can publish to an allowed subject.
2. A principal without publish permission fails before handler dispatch.
3. A principal with fact-write permission can write an allowed fact.
4. A principal without fact-write permission cannot write island facts.
5. Temporary response permission is scoped to one request inbox and deadline.
6. Revoking a grant stops future operations and returns structured
   authorization failures.

Metrics:

- failed authorization count by reason

### E2E-3: Authority Bridge

Purpose: prove dev/prod isolation without merging truth. This can run after
single-island authority checks exist, but it remains part of the MVP proof.

Scenarios:

1. Laptop island imports `gpu.deploy.submit` from prod `deploy.submit`.
2. Laptop can request the imported prod deploy service.
3. Laptop receives exported prod `deploy.<id>.status` stream.
4. Laptop cannot write prod facts directly.
5. Bridge outage causes foreground request failure; it does not queue remote
   mutation intent.

### E2E-4: Fact Replication And Projection

Purpose: prove iroh-docs is the anti-entropy fact source and SQLite is
rebuildable.

Scenarios:

1. Node join fact replicates to all live nodes.
2. Service registration fact appears in remote service registry projections.
3. Route commit fact projects into `gateway.snapshot`.
4. DNS commit fact projects into `dns.snapshot`.
5. Delete `projections.sqlite`; projection rebuilds deterministically from
   docs facts.
6. Drop notification delivery; periodic/full projection pass catches up.
7. Conflicting fact candidates remain reducer-visible, and unauthorized or
   unverified candidates are ignored with visible status.

Metrics:

- fact propagation p50/p95/p99
- projection lag p50/p95/p99
- full rebuild duration by fact count
- snapshot write duration

Current proof status:

- Slice 005 proves rebuildable SQLite projections and snapshots from an
  in-memory fact harness.
- Slice 007 proves one local two-node iroh-docs fact path behind `FactSource`,
  including conflict and unauthorized candidate status.
- Remaining E2E-4 work is remote service registry projection, route/DNS commit
  projection from docs-backed facts, and propagation histograms beyond the
  single local sync scenario.

### E2E-5: Machine Add And Remove

Purpose: prove membership and WireGuard reconciliation.

Scenarios:

1. `ployz init` creates the first island, node facts, bus, docs, blobs,
   projection DB, and local HTTP/DNS serving state.
2. `ployz machine invite` creates a scoped invite with expiration.
3. `ployz join <token>` adds a second node through iroh before WireGuard is
   configured.
4. Ten nodes join and converge to full-mesh WireGuard peer config.
5. Expired invite fails before membership mutation.
6. Graceful remove drains node services, commits route removal, tombstones the
   node, and removes WireGuard peers.
7. Force remove tombstones and excludes the node without waiting for RPC.
8. Tombstoned node reconnect attempts are rejected unless re-invited.

Metrics:

- join duration
- docs convergence duration after join
- WireGuard reconciliation duration
- remove duration
- failed preflight reasons

### E2E-6: Deploy Commit And Drain

Purpose: prove the central deploy invariant.

Scenario manifest:

```text
phase 1: db, stop-start, irreversible
phase 2: web + queue, start-before-cutover
```

Scenarios:

1. Submit through `deploy.submit` queue group and verify one scheduler accepts.
2. Exact planned-node `request_many` capacity probes drive admission before
   mutation.
3. Phase 1 starts DB, verifies readiness, and writes durable phase commit.
4. Phase 2 starts web + queue and does not route until both are ready.
5. Route commit writes a durable fact and projects HTTP/DNS serving state.
6. Drain starts only after the local serving commit fact is durably written.
7. Old instances remain alive during drain grace so stale gateways still have a
   backend.
8. Failure after irreversible phase produces `DeployBlockedAfterIrreversiblePhase`.
9. Cleanup failure after final commit is visible recoverable status, not deploy
   failure.

Metrics:

- capacity fanout duration
- phase duration
- local route commit durability duration
- visible nodes at decision time
- route commit to gateway reload latency
- drain duration

Current proof status:

- Slice 010 adds `deploy-commit-drain-contract`.
- The canary proves `deploy.submit` queue-group acceptance, exact planned-node
  capacity probes, local aggregate serving commit facts, projection/snapshot
  catch-up before old-instance drain/stop, old-backend drain metadata, old
  backend kept alive during projection, cleanup-pending after serving commit,
  coordinator-level irreversible-phase blocking, serving-fact conflict handling
  after an irreversible phase, capacity-field rejection before mutation, forged
  capacity-payload rejection before mutation, projection-proof content mismatch
  rejection, and deterministic serving-head supersession.
- The canary intentionally does not prove real runtime/Docker/ZFS operations,
  WireGuard, real gateway/DNS process restart, or full coordinator
  crash/restart recovery. Those remain E2E-7 and later substrate slices.
- Missing responders are only claimed for selected required participants. Open
  wildcard capacity fanout reports responders as visible-node evidence and does
  not fabricate an unknown missing set.
- A future active-member or partition-view check can improve that visibility
  evidence before mutation, but this scenario deliberately has no quorum,
  witness-ack, or peer-ack commit boundary.

### E2E-6a: Advisory Lease-Fenced Commands

Purpose: prove product features can express advisory ownership and epoch fencing
without importing a NATS server lock topology or pretending iroh-docs is a
consensus system.

Scenarios:

1. First holder acquires an advisory lease on the connected node and the command
   result reports visible nodes at decision time.
2. A second holder receives a structured conflict before mutation while the
   first lease is active.
3. Renewal extends the current holder's lease.
4. Expiry allows a new holder at a higher epoch.
5. Stale guards cannot mutate after a newer epoch exists.
6. Conflicting same-epoch claims remain candidates, reduce deterministically by
   `(epoch desc, content_hash asc)`, and annotate the loser as superseded.
7. ACME HTTP-01 challenge state can only be published or deleted by the current
   local winner's fencing epoch and claim hash.
8. Dropping a local lease guard records a best-effort release without claiming
   cluster-wide exclusivity.
9. A local-only command can acquire and publish with zero visible peer witnesses;
   the visible-node set is evidence, not a commit gate.

Current proof status:

- Slice 009 adds `lease-acme-contract` against the corrected advisory lease
  contract: no witness acks, no quorum, no strict mode, and no pin-fact commit
  phase.
- The current in-memory canary proves visible nodes at decision time,
  structured active-holder conflict, expiry takeover, stale publish/delete
  rejection with state preservation, deterministic supersession, same-epoch
  loser fencing, local-only command success, and best-effort drop release.
- Remaining work is docs-backed lease facts over real iroh replication and
  gateway HTTP challenge serving.

Metrics:

- acquisition duration,
- contention count,
- stale mutation rejection count,
- superseded candidate count,
- visible nodes at decision time,
- takeover duration after expiry.

### E2E-7: Crash And Restart

Purpose: prove control-plane restarts do not break the data plane.

Scenarios:

1. Kill daemon before first phase commit; restart adopts candidates or cleans
   them as explicit recoverable state.
2. Kill daemon after phase commit before drain; restart rebuilds projection and
   resumes drain.
3. Kill coordinator during drain; HTTP/DNS serving keeps serving last good
   state and old instances remain reachable during drain grace.
4. Restart HTTP serving while daemon is down; it loads last good state and
   serves.
5. Restart DNS serving while daemon is down; it loads last good state and
   serves.
6. Delete projection DB while HTTP/DNS are serving; daemon rebuilds and
   publishes fresh serving state without traffic interruption.
7. Coordinator dies permanently after local route commit before other nodes see
   the fact; the connected node reports the local commit and visible nodes at
   decision time, and other nodes converge through eventual docs replication.
8. Coordinator is down while service-to-service traffic crosses nodes over
   last-applied WireGuard config; traffic continues and the node exposes
   coordinator health as stale/unavailable for mutations.

Metrics:

- data-plane request success during daemon outage
- restart adoption duration
- projection rebuild duration
- stale snapshot age reported to operator

### E2E-8: Scale And Reliability Harness

Purpose: prove the architecture has margin under large load.

Scenarios:

1. 200 logical nodes subscribe to `gateway.changed`; publish once and measure
   all wakeups.
2. 1,000 logical nodes subscribe to `gateway.changed`; publish once and measure
   all wakeups.
3. `request_many node.*.capacity` against 200 logical nodes.
4. `request_many node.*.capacity` against 1,000 logical nodes.
5. Run 100 deploys with randomized node failures before/after commit.
6. Run with packet delay/drop simulation at the bus layer.
7. Run 10,000 logical-node synthetic bus test as an MVP stress gate.

Metrics:

- memory per logical node
- CPU during fanout
- request-many p99
- missing responder accounting accuracy
- convergence tail latency
- 10,000-node synthetic test pass/fail and resource envelope

### E2E-9: Semantic Leverage

Purpose: prove the new primitives reduce glue and expose business logic.

Scenarios:

1. Reimplement a small machine join/add flow from the old foundation and compare
   the shape of code and tests.
2. Reimplement a route commit to HTTP/DNS serving-state flow and compare the
   shape of code and tests.
3. Reimplement the deploy commit-before-drain invariant and compare the shape of
   code and tests.
4. Reimplement ACME challenge ownership as a lease-fenced canary and compare it
   against the old cert coordination path.
5. Add one new business rule after those flows exist and measure how many files
   and abstraction layers change.

Metrics:

- feature logic lines versus substrate glue lines,
- number of files touched to add a business rule,
- number of public types or enum variants added for one feature,
- number of tests that assert product behavior directly,
- review notes on whether business invariants are visible without reading
  transport/storage internals.

## Required Test Artifacts

Each slice should add or update:

- deterministic test scenario code,
- a structured metrics output file or snapshot,
- clear failure assertions, not just logs,
- an MVP-local documented command for local execution.

Candidate MVP-local commands:

```text
cd MVP && cargo test
cd MVP && cargo run -p mvp-e2e -- bus-contract
cd MVP && cargo run -p mvp-e2e -- scale
```

Final MVP gate:

```text
cd MVP && cargo test
cd MVP && cargo run -p mvp-e2e -- all
cd MVP && cargo run -p mvp-e2e -- scale
```

Do not add root `justfile` targets while the rewrite is isolated under `MVP/`.
When the MVP is later migrated into the main workspace, add repo-level commands
as part of that explicit migration.
