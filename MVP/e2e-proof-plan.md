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

Purpose: prove signed p2panda operations are the durable fact source and
SQLite is rebuildable.

Scenarios:

1. Node join fact replicates to all live nodes.
2. Service registration fact appears in remote service registry projections.
3. Route commit fact projects into `gateway.snapshot`.
4. DNS commit fact projects into `dns.snapshot`.
5. Delete `projections.sqlite`; projection rebuilds deterministically from
   signed p2panda operations.
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
- Slice 018b adds `p2panda-fact-source-contract`, proving existing reducers can
  consume p2panda-backed fact candidates, rebuild deleted SQLite, write
  gateway/DNS snapshots, and surface conflict candidates without changing
  reducer business logic.
- Slice 018c extends the p2panda proof with operation export/import. A store can
  export signed operations, another same-island store can import them, duplicate
  imports are idempotent, same-key/different-content imports remain conflict
  candidates, imported operations must match a trusted author key for the
  claimed principal, and reader permissions still control candidate status and
  payload visibility. Payload reads require exact stored fact identity before
  using content-hash storage.
- Slice 019b adds persistent p2panda SQLite storage and
  `p2panda-process-role-serving-contract`, proving Ployz indexes rebuild from
  the p2panda operation log and serving projection can use the persistent store
  while preserving last-good gateway/DNS state.
- Slice 020 adds `p2panda-sync-fact-source-contract`. Two persistent p2panda
  stores exchange missing signed operations through `p2panda-sync`, not manual
  operation copying; the remote store projects node/service/route/DNS facts
  into SQLite and gateway/DNS snapshots; deleting projection SQLite rebuilds
  from synced p2panda operations; repeated sync is a no-op; same-key races
  remain conflict candidates; sync egress requires trusted same-island replica
  sessions; and payload reads still require read grants. The same scenario
  records exact 200/1,000/10,000 sync/import convergence using in-memory stores
  for the load probe.
- Slice 021 adds `p2panda-acme-http01-contract`. ACME lease/challenge facts are
  written as p2panda operations on one local node, replicated through
  `sync_panda_fact_stores`, projected on a second local node, and served
  through the HTTP-01 wire proof. Scoped ACME grants, trusted replica sessions,
  stale synced candidates, no-op repeat sync, and deleted-SQLite rebuild are
  all part of the scenario.
- Slice 022 adds `p2panda-net-sync-contract`. Local p2panda-net nodes exchange
  opaque stable `PandaFactOperation` envelopes over the maintained
  iroh/gossip/log-sync stack; the receiver imports those envelopes through the
  canonical Ployz validation path before projection can see them. The scenario
  records six transported operations, three inserted/conflict imports, one
  duplicate no-op, two conflict candidates, explicit untrusted-author and
  cross-island rejection, trusted-replica import gating, zero cross-island
  leakage, a 9ms projection rebuild, and 80ms network sync in the latest full
  all-run.
- Slice 023 adds `p2panda-net-owned-node-contract`. The scenario uses owned
  p2panda-net nodes rather than `test_utils`, imports through the shared
  transport fact driver, proves trusted-replica gating with a known envelope
  instead of depending on network delivery order, rejects untrusted author,
  cross-island, and malformed envelopes, and projects only the valid
  non-conflicting node fact.
- Slice 030 adds `p2panda-net-fact-node-contract`. The scenario gives the
  receiver a running p2panda-net fact node and projects from that node's local
  `SharedPandaFactStore`; the E2E no longer performs the main success-path
  import by collecting `Vec<Vec<u8>>` after network transport. It records 11
  attempted imports, six inserted operations, one duplicate, one conflict,
  three rejected operations, one projected node/service/gateway/DNS set, live
  sync/import timing, gateway/DNS snapshot loading, and deleted-SQLite
  projection rebuild from the synced receiver store.
- Slice 024 extracts ACME claim/present/clear into `mvp-acme-command`. The
  p2panda ACME and p2panda-net ACME scenarios still prove the same transport,
  projection, last-good serving, scoped-grant, stale-write, trusted-replica,
  and SQLite rebuild behavior, but the command semantics now live in reusable
  business code instead of the E2E fixture.
- Remaining E2E-4 work is continuous propagation histograms once multiple
  process roles exchange p2panda-net traffic, plus production shutdown/status
  hardening beyond drop-based local-node cleanup.

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

Current proof status:

- Slice 014 adds `membership-wireguard-contract`.
- The scenario proves docs-backed membership joins through `mvp-iroh`, ten
  joined nodes converging into 90 full-mesh peer relationships, expired invite
  rejection before mutation, split join/tombstone fact grants, tombstone
  projection down to nine live nodes, and tombstoned rejoin rejection derived
  from replicated tombstone facts.
- The WireGuard proof is a last-applied config/data-plane harness, not kernel
  WireGuard. Loopback TCP service-to-service traffic is gated by the applied
  peer table, tombstoned peers are rejected before opening a service
  connection, and peer endpoints come from the applied snapshot rather than
  caller-supplied addresses.
- Slice 017 adds `machine-remove-contract`.
- The graceful-remove proof uses four logical nodes: a target, remaining source
  node, remaining destination node, and operator/observer. Membership/removal
  facts are docs-backed through `mvp-iroh`; serving cutover still uses the
  current bus-backed serving fact writer; projection reads both through one
  combined `FactSource` for the scenario. The command probes the target before
  mutation, writes `NodeRemovalStarted`, requires
  `NoNewWorkAndDrained`, commits route removal, projects serving catch-up,
  stops removed workloads, tombstones the node, rebuilds projection from facts,
  replans the last-applied mesh snapshot, proves remaining source-to-destination
  traffic still works, and rejects traffic to the removed target from the
  applied peer table.
- Slice 029 extends `machine-remove-contract` with coordinator restart
  recovery after serving commit and before stop/tombstone. The command writes a
  machine-remove decision fact before mutation, drops the original coordinator
  and in-memory pending value, replays p2panda operations into a fresh store,
  reconstructs pending cleanup from facts, proves probe/drain/serving writes
  are not replayed, gates stop on `ProjectionCatchUp`, writes tombstone plus
  cleanup-done, and proves a second recovery completes without RPC.
- Remaining E2E-5 work is real host/container WireGuard interface mutation,
  real runtime workload stop/transfer backends behind the same participant
  contract, and a production join/remove RPC path over iroh/PloyzBus.

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
  WireGuard, or real gateway/DNS process restart. Slice 018c adds the
  coordinator restart proof after serving commit. Slice 023 adds the
  pre-serving candidate cleanup ABI proof.
- Slice 023 adds `deploy-candidate-cleanup-contract`. A reversible failure
  after prepare/start returns `DeployError::PreCommitFailed` with visible nodes,
  attempted candidate targets, and structured cleanup status; old-backend
  drain/stop counts stay zero before serving commit; explicit recovery from
  decision/no-serving-commit cleans planned candidates without rerunning
  prepare/start.
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
- Slice 021 adds the p2panda-backed ACME canary over the Slice 020 sync
  boundary. It proves advisory lease conflict-at-entry, scoped challenge
  grants, release facts, visible nodes in every command result, deterministic
  superseded loser reporting, and gateway HTTP-01 serving from the synced
  projection.
- Remaining work for E2E-6a is applying the same lease-fenced command shape to
  another real singleton resource after volume ownership, such as machine
  mutation.
- Slice 027 adds `volume-transfer-contract`. It proves the lease-fenced command
  shape for volume ownership: current ownership and lease facts are read before
  mutation, active holder conflict fails before participant RPC, a durable lease
  claim is written before snapshot/receive, ownership write authorization is
  preflighted before lease write or participant RPC, stale holder/expired lease
  mutation fails before ownership commit, concurrent ownership races fail before
  stale commit, and the command result reports visible nodes at decision time.

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
   decision time, and other nodes converge through eventual p2panda operation
   sync.
8. Coordinator is down while service-to-service traffic crosses nodes over
   last-applied WireGuard config; traffic continues and the node exposes
   coordinator health as stale/unavailable for mutations.

Metrics:

- data-plane request success during daemon outage
- restart adoption duration
- projection rebuild duration
- stale snapshot age reported to operator

Current proof status:

- Slice 011 adds `steady-state-serving-contract`.
- The scenario proves the semantic serving-role boundary: typed gateway/DNS
  queries load from actor-owned last-good snapshot state while the local
  coordinator is absent for mutations.
- A separate harness serving-commit writer can publish a commit, project it
  locally, and reload it into serving state without constructing a local deploy
  coordinator.
- Deleting `projections.sqlite` does not interrupt serving queries. A fresh
  projection actor rebuilds from facts, and serving is queried while that
  rebuild task is in flight.
- A serving actor restart from snapshot files works while the coordinator is
  still absent.
- Corrupt, wrong-island, deleted, and symlinked next snapshots preserve
  last-good answers and record structured reload failure status.
- Slice 012 adds `process-role-serving-contract`.
- The scenario proves real OS process fate separation inside the MVP harness:
  a local coordinator process writes the first serving commit, the
  serving/projection process loads it, the parent kills the coordinator, and
  typed gateway/DNS queries keep answering before any later recovery command
  runs.
- Local mutation attempts through the killed coordinator path fail visibly,
  while the serving/projection role reports its own health and
  `mutation_unavailable_in_this_role` without polling coordinator liveness.
- A separate remote-replication injector writes a later already-authorized
  serving fact. The still-running serving/projection process projects and
  reloads it without reviving local mutation authority.
- Deleting `projections.sqlite` and rebuilding projection state is now proven
  inside the long-lived serving/projection OS process, and that process can
  restart from snapshot files while no coordinator process is running.
- Slice 013 adds `wire-serving-contract`.
- The scenario proves real HTTP and DNS wire serving inside separate process
  roles: HTTP routes by `Host` through a deterministic loopback backend, DNS
  answers real UDP AAAA queries, and both keep serving after the local
  coordinator process is killed.
- A later already-authorized serving fact projects and reloads into HTTP/DNS
  roles while the coordinator remains dead. Local mutation attempts through the
  killed coordinator still fail visibly.
- Corrupt, missing, and wrong-island next snapshots fail explicit wire-role
  reload, preserve last-good HTTP/DNS answers, and surface structured
  last-good-after-failure status.
- Deleting `projections.sqlite` while HTTP/DNS are live is now proven at the
  wire level. A fresh remote serving fact is injected before rebuild, the
  projection rebuild publishes fresh snapshots, and explicit wire reload moves
  HTTP/DNS answers to the rebuilt serving state.
- HTTP and DNS wire roles can restart while the coordinator is still dead and
  load snapshot files before answering wire requests.
- Slice 014 adds `membership-wireguard-contract`.
- The scenario proves service-to-service traffic through a last-applied mesh
  data-plane process before coordinator death, after the coordinator is killed,
  and after the data-plane role restarts while the coordinator remains dead.
  Local mutation attempts through the killed coordinator fail visibly.
- Slice 018c adds `deploy-restart-recovery-contract`.
- The scenario proves the deploy coordinator can be dropped after the
  p2panda-backed serving commit is durable and before drain starts. The proof
  exports the surviving p2panda operations, imports them into a fresh fact
  store, and a fresh coordinator recovers the deploy decision plus serving
  commit from that imported fact source. Recovery requires `ProjectionCatchUp`,
  resumes drain/stop, writes cleanup-done, and a later recovery returns
  complete without RPC.
- The scenario also proves typed gateway/DNS serving keeps answering from
  last-good snapshots while the coordinator object is absent, no
  capacity/prepare/start participant work is replayed after restart,
  cleanup-pending after restart carries visible nodes plus serving commit id,
  and deploy decision, serving commit, and cleanup-done facts live in one fact
  substrate for the proof.
- Slice 026 extracts the p2panda deploy/serving fact writers and p2panda
  `FactSource` wrapper from this E2E into `mvp-deploy-p2panda`. The restart
  scenario still owns process choreography and operation export/import, but it
  no longer owns deploy-specific p2panda outcome mapping.
- Slice 029 adds the machine-remove equivalent recovery proof. The target
  command coordinator is dropped after the p2panda-backed serving cutover and
  before stop/tombstone. A fresh store imports the surviving operations through
  trusted replica authority; recovery reads the decision and exact serving
  commit, returns pending cleanup, still requires projection catch-up before
  stop, and later observes cleanup-done without contacting the participant.
  Remaining mesh traffic continues after the coordinator outage/recovery.
- Slice 021 adds coordinator/issuer absence to the ACME serving path. HTTP-01
  continues serving the last-good challenge after the command adapter is
  dropped, a later p2panda sync/rebuild clears the challenge explicitly, stale
  synced lower-epoch facts cannot roll serving back from the takeover winner,
  and deleting `projections.sqlite` rebuilds from the synced p2panda store.
- Slice 023 adds `p2panda-net-acme-http01-contract`. ACME lease/challenge facts
  move over owned p2panda-net nodes, import through the canonical
  trusted-replica path, project on the receiving node, serve HTTP-01 while the
  issuer adapter is absent, clear to 404 after a transported clear fact, and
  rebuild SQLite from transported p2panda operations.
- Slice 023 adds the explicit pre-serving/pre-commit deploy cleanup ABI proof.
- Slice 030 proves a running p2panda-net fact node can ingest into a local
  `SharedPandaFactStore` and rebuild serving projections from that store. This
  is still in-process local-node proof; remaining E2E-7 work is process-role
  p2panda-net serving replication, production WireGuard adapter proof, and
  replacing the HTTP/DNS fallback crates with Pingora/`hickory-server` if those
  become the chosen production protocol primitives.

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

Current proof status:

- Slice 016 removes parallel node identity and visible-node evidence types
  before the next node-facing product command. Deploy, lease/ACME, mesh, and
  E2E fixtures now use one shared `NodeId`/`VisibleNodes` type, and WireGuard
  peer snapshots use typed routing fields instead of raw strings. The proof is
  existing behavior staying green with fewer representations to choose from.
- Slice 021 keeps ACME business logic small enough to inspect directly:
  acquire advisory lease, present challenge, project/serve, clear, takeover,
  and reject stale/conflicting writes. The new p2panda work reused the generic
  sync/import/projection boundaries instead of adding ACME-specific replication
  code. The old-code baseline remains `crates/ployz-cert-backends` plus
  `crates/ployzd/src/daemon/cert_coordination.rs`; the MVP comparison should
  count the E2E-local adapter and focused domain tests, not placeholder Hyper
  serving polish.
- Slice 024 turns that E2E-local ACME adapter into `mvp-acme-command`. The old
  cert coordination/backend baseline is 1,055 LOC; the reusable command surface
  is 659 LOC plus focused tests, and the p2panda ACME E2E drops from roughly
  1,653 LOC to 1,218 LOC while preserving the same canary behavior.
- Slice 025 is a maintenance-surface win, not a product-command win. It removes
  direct git p2panda dependencies from `mvp-e2e`, deletes the ACME-local
  p2panda-net harness, moves p2panda-net wire transport into
  `mvp-p2panda-transport`, keeps test wire movement behind that crate's
  `harness` feature, and deletes the 396-line `mvp-p2panda-spike` source after
  production-shaped p2panda fact tests cover its behaviors.
- Slice 026 is the deploy equivalent of Slice 024's ACME extraction. The
  restart-recovery E2E shrinks from 945 to 789 lines and stops carrying
  deploy-specific p2panda writer/outcome glue; `mvp-deploy-p2panda` is the
  reusable 492-line adapter plus focused tests.
- The routing-owned serving commit correction moves the serving writer contract
  from deploy to routing and updates machine remove to consume the same writer
  seam. This is a maintenance-leverage proof rather than a new product proof:
  serving cutover facts now have one owner, deploy loses duplicate serving
  writer code, and machine remove no longer bypasses routing's writer
  semantics.
- A read-only LOC check after Slice 026 says the old
  `crates/ployzd/src/daemon/handlers/deploy.rs` baseline is 4,558 lines. The
  MVP deploy domain/coordinator plus p2panda adapter is roughly 2,700 lines
  before tests, with deploy E2Es carrying the product proofs separately. This
  is a real feature-surface win, but the MVP foundation itself is already
  large, so future slices must track how much new shared substrate they add.
- Slice 022 is a mixed leverage result. It adds a 430-line E2E transport proof
  and 84 lines of fact-store envelope/test surface, but it prevents a worse
  fork: p2panda-net is now proven as the carrier while Ployz keeps one canonical
  import/authority path. No product business logic moved; the semantic gain is
  deleting the future need to hand-roll iroh transport for fact sync, not yet
  shrinking the current stable `PandaFactStore` implementation.
- Slice 027 is the first volume movement canary. The old reference surface
  across volume transfer/listener/API/smoke test is 1,707 LOC. The MVP adds
  about 1,150 lines of `mvp-volume` domain/command code, about 800 lines of
  focused unit tests, and about 1,200 lines of p2panda-backed E2E harness. The
  result is yellow-green but not a raw LOC reduction: ownership and fencing
  semantics are visible in typed command code, and no new generic substrate was
  added, but the E2E-local p2panda harness is still large and should only be
  extracted after a second volume/storage command repeats it.
- Slice 028 moves machine remove from a mixed iroh-docs/bus fact proof to a
  single p2panda-backed fact source. Raw LOC increases: the E2E grows from 934
  to 1,037 LOC and `mvp-machine-p2panda` adds 666 LOC including tests. The
  leverage win is boundary clarity rather than line count: `DocsMachineFactWriter`
  and `CombinedFactSource` are gone, joined-node/removal/tombstone/serving facts
  rebuild from one p2panda store, and scoped author checks are explicit.
- Slice 029 is a mixed implementation/reuse win. Machine remove gains durable
  restart recovery facts and E2E proof without adding a generic workflow
  engine; the business rule is still visible as probe, decision, serving
  commit, projection catch-up, stop, tombstone, cleanup-done. At the same time,
  repeated p2panda wrapper mechanics moved into `SharedPandaFactStore`, so the
  next command should not need another local `Arc<Mutex<PandaFactStore>>`
  adapter shell.

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
