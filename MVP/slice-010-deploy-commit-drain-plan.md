---
title: Slice 010 Deploy Commit Before Drain Plan
status: active
created: 2026-05-17
origin:
  - VISION.md
  - docs/architecture.md
  - docs/routing-and-deploys.md
  - docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md
  - docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md
  - docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-005-fact-projection-plan.md
  - MVP/slice-009-advisory-lease-acme.md
---

# Slice 010 Deploy Commit Before Drain Plan

## Problem Frame

This slice proves the central deploy invariant on the new MVP primitives:

> Route cutover is a local durable fact; drain is a consequence of that fact.

The goal is not to port the old deploy handler. The old deploy code is the
semantic-leverage baseline to beat, not the design to preserve. This slice
should rebuild the smallest deploy state machine that exercises the MVP bus,
fact, projection, and actor boundaries:

```text
deploy.submit queue
  -> request_many node.*.capacity
  -> phase prepare/start/ready
  -> local durable route/gateway/DNS facts (serving commit)
  -> projection/snapshot catch-up while old backends stay alive
  -> old-instance drain/stop cleanup
  -> cleanup status
```

The command consistency boundary is still the operator's connected node. The
deploy coordinator writes commit facts durably to the connected node and
returns command-visible evidence. It must not wait for witness acknowledgements,
`store.pin_fact`, `min_replicas`, or a hidden quorum.

## Requirements Traceability

- `VISION.md`: deploy is a foreground primitive with visible preconditions,
  bounded effects, clear results, and explicit verification. The daemon is
  disposable; the data plane must outlive it.
- `docs/architecture.md`: mutating commands inspect intent and live
  preconditions, fail before mutation when preconditions are missing, commit at
  the point of no return, and report cleanup or partial progress explicitly.
- `docs/routing-and-deploys.md`: gateway and DNS are projections; after a final
  commit, cleanup failure is visible state, not deploy failure.
- `docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md`:
  drain is command-shaped deploy input, not background reconciliation. Local
  mutation and remote coordination must stay target-aware.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`:
  validate final participants before the first mutation and return structured,
  audience-aware preflight failures.
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`:
  live observation failures are evidence for the command result; they must not
  rewrite stored truth.
- `MVP/architecture.md`: `DeployCoordinatorActor` owns deploy state machines and
  durable commit boundaries; projection and serving-state appliers are separate
  steady-state responsibilities.
- `MVP/e2e-proof-plan.md`: E2E-6 requires deploy submit queueing, capacity
  fanout, phase readiness, route commit before drain, old backends kept alive
  during drain grace, irreversible-phase failure, and cleanup-pending status.
- `MVP/primitive-decisions.md`: fact writes are local durable commits with
  eventual replication; visible nodes are decision-time evidence, not a commit
  gate.
- `MVP/slice-005-fact-projection-plan.md`: projection correctness cannot depend
  on notifications or SQLite; reducers and snapshots are the serving-state
  boundary.
- `MVP/slice-009-advisory-lease-acme.md`: advisory leases are available if the
  deploy slice proves a need, but they are not a strict ownership primitive and
  should not be introduced just to hide deploy races.

## Scope

Implement a self-contained MVP deploy canary under `MVP/`:

- a new `mvp-deploy` crate with typed deploy IDs, phase IDs, instance IDs,
  route commit IDs, command context, visible-node evidence, state-machine
  transitions, participant requests, and structured deploy outcomes,
- a small actor-facing coordinator API that uses `BusActorHandle` for
  `deploy.submit`, `request_many node.*.capacity`, and participant commands,
- local durable fact writes for route, gateway, and DNS commits using the
  existing fact/projection contracts,
- future-recovery-ready serving facts: after serving commit, the old backends to
  drain are represented durably even though full crash/restart recovery remains
  E2E-7 work,
- an E2E scenario named `deploy-commit-drain-contract`,
- metrics showing fanout, phase, commit-to-projection, and drain timings,
- a slice report comparing the new MVP deploy canary against the old deploy
  LOC and, more importantly, against the old semantic shape.

Out of scope for this slice:

- real Docker/runtime/ZFS operations,
- real gateway or DNS process restarts,
- WireGuard reconciliation,
- rollback,
- machine add/remove,
- docs-backed lease persistence,
- full coordinator crash/restart recovery,
- durable per-backend cleanup progress facts,
- strict deploy ownership under partition,
- `store.pin_fact`, witness acknowledgements, or quorum durability,
- a generic workflow engine or global desired-state reconciler.

The slice may simulate participant runtime behavior in memory, but the
simulation must expose product-shaped participant commands and statuses. Tests
should not depend on private coordinator internals.

## Future Active-Member Evidence

The operator may later want a member list or active-partition view so commands
can say, "I checked the currently alive members I know about." That is a useful
future primitive, but it is intentionally not part of this slice's commit
contract.

For Slice 010, visible nodes remain evidence in the command result:

- capacity fanout reports who replied,
- wildcard capacity fanout does not invent a missing set when there is no
  expected-member input,
- no-responder/timeout on a selected required participant is a structured
  foreground failure before serving commit and cleanup-pending evidence after
  serving commit,
- route/gateway/DNS facts commit locally on the connected node,
- replication converges later,
- no peer response is required to make a local commit durable.

If active-member evidence is added later, it should enrich preflight and command
results. It must not become a hidden peer-ack commit protocol.

## Commit Vocabulary And Drain Gate

This slice uses these terms consistently:

- `PhaseCommit`: a deploy lifecycle commit for phase-owned work. In the sample
  manifest, phase 1 DB commit is irreversible. Failure after this point and
  before serving commit becomes `DeployBlockedAfterIrreversiblePhase`.
- `ServingCommit`: the local durable write of route, gateway, and DNS commit
  facts for a phase that changes serving state. This is the route cutover fact
  and the drain gate. It is complete when the connected node accepts those fact
  writes locally.
- `ProjectionCatchUp`: a local projection pass that proves gateway/DNS snapshots
  can be derived from the serving commit. Projection is evidence for serving and
  tests; it is not authority and it must not roll back a serving commit.
- `DrainStart`: old backends have been removed from the active `backends` set by
  the serving commit and are listed in `old_backends_to_drain`; participant
  drain commands may now be sent.
- `CleanupDone` / `CleanupPending`: the post-serving-commit old-instance drain
  and stop outcome. Cleanup failure after serving commit does not make the
  deploy fail; it leaves operator-visible cleanup status.

The exact gate for deploy drain is `ServingCommit`. The E2E canary should also
run `ProjectionCatchUp` before stopping old backends, so it proves the local
serving role can derive the new snapshot while old backends are still alive.

## Concurrent Commit Contract

Slice 010 must not prove only the happy path where one coordinator exists.

Rules:

- Command entry reads visible deploy/serving facts before the first mutation. If
  a conflicting active deploy or serving commit is already visible, it fails
  with a structured conflict before writing new facts.
- If two connected nodes race and both write locally before replication
  converges, both facts remain candidates.
- The reducer selects the surviving serving/deploy candidate by
  `(epoch desc, content_hash asc)` and annotates the loser as `Superseded` for
  operator status.
- No operator-picks path is introduced.

Current projection route/gateway facts are usable for the serving-state proof,
but the existing projection status surface may need a narrow reducer/status
extension to report supersession instead of generic conflict. That is in scope
only for deterministic deploy/serving-head selection; adding new route/gateway
payload fields is not.

## Existing Patterns To Follow

- `MVP/bus/src/actor.rs`: business-facing async code uses `BusActorHandle`.
  Direct `MemoryBus` access stays harness-only.
- `MVP/bus/src/message.rs`: request/reply code uses `Payload`,
  `RequestTarget::Pattern`, `RequestManyPolicy`, and `ResponseMessage`.
- `MVP/bus/src/grants.rs`: deploy tests should grant only the publish,
  subscribe, queue, request, and fact capabilities each principal needs.
- `MVP/e2e/src/actor_contract.rs`: existing `deploy.submit` queue group and
  `node.*.capacity` request-many patterns are the nearest bus examples.
- `MVP/projection/src/facts.rs`: `RouteCommitFact` already carries
  `old_backends_to_drain`; `GatewayCommitFact` and `DnsCommitFact` already feed
  snapshots.
- `MVP/projection/src/reducer.rs`: reducers are deterministic and reject
  malformed payload/key mismatches; deploy should not bypass this path.
- `MVP/projection/src/source.rs`: `FactSource` is the projection seam. Deploy
  logic should write facts through the bus/fact boundary, not call projection
  reducers directly as authority.
- `MVP/e2e/src/projection_contract.rs`: seed facts, run projection actors,
  check snapshots, and write metrics in the existing scenario style.
- `MVP/e2e/src/metrics.rs`: scenario artifacts belong under
  `MVP/target/mvp-e2e/<scenario>/`.

Important naming warning: `BusActorHandle::drain()` is bus-runtime drain. It is
not old-instance deploy drain. Deploy drain needs its own typed participant
operation.

Current API warning: projection currently has a synchronous
`BusFactSource::new(InMemoryBus)` while business-facing code uses
`BusActorHandle`. The slice should add a harness-only constructor that returns a
`BusActorHandle`, `BusAuthority`, and matching `InMemoryBus` clone for
projection wiring, or an equivalent harness-only wrapper. Deploy business code
still consumes the actor handle; the raw in-memory bus remains an E2E/projection
fixture.

## Crate Scout

Checked before planning:

- `statig` provides hierarchical state machines, optional macros, async
  handlers, and state-local storage. It is useful when the statechart itself is
  the main complexity, but this slice's complexity is the product invariant and
  fact/projection boundary. Decision: defer.
  <https://docs.rs/statig>
- `rust-fsm` provides a DSL and `StateMachineImpl` trait for strict state
  machines. It is clean for static transition tables, but the MVP deploy model
  needs dynamic phases, participant evidence, and resumable command outcomes.
  Decision: defer.
  <https://docs.rs/rust-fsm/>
- `sm` offers macro-defined compile-time transition checking. That is attractive
  for small closed machines, but awkward for persisted/replayed deploy state,
  dynamic phase lists, and structured recovery. Decision: defer.
  <https://docs.rs/sm>
- `petgraph` is a mature graph library with graph types and algorithms. It may
  become useful when deploy manifests have real dependency DAGs. Slice 010 has
  two explicit phases, so a graph dependency is unnecessary. Decision: defer.
  <https://docs.rs/petgraph/>

Use existing MVP dependencies first:

- `thiserror` for structured deploy error enums if hand-written impls become
  noisy,
- `serde`/`serde_json` for harness payloads and fact payloads,
- `tokio`/Kameo through the existing actor patterns,
- `mvp-bus` for request/reply, queue groups, grants, and fact writes,
- `mvp-projection` for route/gateway/DNS facts and snapshots.

The planned implementation should start with explicit Rust enums and transition
methods. That matches the repo guardrails: state machines are enums plus
transition methods, variant data lives in variants, and callers branch on
structured outcomes.

Revisit this choice if the implementation needs cross-phase guard tables,
parallel substates, or persisted replay tables that make the explicit enum
model less readable than a small state-machine crate.

## Implementation Units

### 1. Deploy Domain Crate

Files:

- `MVP/Cargo.toml`
- `MVP/deploy/Cargo.toml`
- `MVP/deploy/src/lib.rs`
- `MVP/deploy/src/domain.rs`
- `MVP/deploy/src/error.rs`
- `MVP/deploy/src/state_machine.rs`
- `MVP/deploy/src/tests.rs`

Decisions:

- Newtypes for `DeployId`, `PhaseId`, `InstanceId`, `RevisionId`,
  `RouteCommitId`, and `NodeId`/backend references at the deploy boundary.
- Model phase state and deploy outcome separately. Phase state variants should
  stay phase-local, such as `Planned`, `Preparing`, `Ready`, `Committed`,
  `Draining`, and `Done`. Deploy-level outcomes include
  `DeployDone`, `FailedBeforeCommit`, `DeployBlockedAfterIrreversiblePhase`,
  and `CleanupPending`.
- Model command output separately from durable state:
  `DeployCommandResult` includes visible nodes, committed route IDs, cleanup
  status, and structured warnings.
- Local durable fact write is the commit boundary. There is no field for
  `min_replicas`.
- The domain records enough serving-commit inputs that future E2E-7 recovery can
  re-derive old backends to drain without inventing a route head from live
  observation.

Test scenarios:

- A route/drain transition before route commit is rejected by a structured
  transition error.
- Phase 1 DB commit requires readiness evidence.
- Phase 2 route commit requires web and queue readiness evidence.
- Failure before `PhaseCommit` aborts without route facts or drain work.
- Failure after an irreversible phase returns
  `DeployBlockedAfterIrreversiblePhase`.
- Cleanup failure after `ServingCommit` returns `CleanupPending` and preserves
  the route commit as success.
- Command results always include visible nodes at decision time, even when the
  visible set is empty.

### 2. Deploy Coordinator Actor Boundary

Files:

- `MVP/deploy/src/coordinator.rs`
- `MVP/deploy/src/participant.rs`
- `MVP/deploy/src/wire.rs`
- `MVP/deploy/src/tests.rs`

Decisions:

- `DeployCoordinator` owns deploy state and participant orchestration.
- The coordinator talks to the world through `BusActorHandle` and typed payload
  adapters, not through direct `MemoryBus` internals.
- `deploy.submit` is served through a queue group such as `schedulers`.
- Capacity inspection uses `request_many` with `RequestTarget::Pattern` over
  `node.*.capacity`.
- Capacity fanout records responders as visible nodes. It does not claim to know
  which wildcard subscribers were missing unless the command already has an
  explicit expected participant list.
- Participant operations use explicit subjects such as
  `node.<node_id>.rpc.prepare_instance`, `node.<node_id>.rpc.start_instance`,
  `node.<node_id>.rpc.drain_instance`, and
  `node.<node_id>.rpc.stop_instance`.
- No responder and timeout errors are foreground deploy failures before commit,
  or recoverable cleanup statuses after `ServingCommit`.

Test scenarios:

- Two scheduler subscribers in the same queue group accept exactly one
  `deploy.submit`.
- Capacity fanout records all responders as visible nodes and does not fabricate
  missing responders for an open-ended wildcard request.
- A missing required participant before commit fails before mutation.
- A missing old-backend drain responder after commit becomes cleanup-pending
  status, not deploy failure.
- The coordinator never holds mutable deploy state across a long participant
  request in a way that blocks unrelated bus operations.

### 3. Serving-State Commit Integration

Files:

- `MVP/deploy/src/serving_commit.rs`
- `MVP/deploy/src/tests.rs`
- `MVP/projection/src/reducer.rs` only if deterministic supersession for
  serving heads requires reducer changes
- `MVP/projection/src/model.rs` only if projection status needs to distinguish
  `Superseded` from generic conflict

Decisions:

- Use existing `ProjectionFactPayload::RouteCommit`,
  `ProjectionFactPayload::GatewayCommit`, and
  `ProjectionFactPayload::DnsCommit` for the first proof.
- Do not add route/gateway/DNS payload fields in this slice.
- A route commit includes new backends and `old_backends_to_drain`.
- Gateway/DNS commit facts are written only after phase readiness succeeds and
  become the `ServingCommit`.
- Old backends remain alive until the drain phase completes.
- Snapshot/projection is evidence that serving state can be derived from facts;
  it is not deploy authority.
- Per-backend cleanup progress is not durable in this slice. Future restart
  recovery can re-drain old backends idempotently from `old_backends_to_drain`,
  or add explicit cleanup facts in E2E-7.

Test scenarios:

- Route, gateway, and DNS fact keys are deterministic and mostly immutable.
- Route commit payload contains old backends to drain.
- Gateway projection contains new backends in active `backends` and old backends
  only in `old_backends_to_drain`; old backends are not accidentally active
  after serving commit.
- DNS projection updates from the committed DNS fact.
- Rebuilding projection from facts after deleting SQLite produces the same
  serving state used for drain decisions.
- Concurrent same-route serving commit candidates reduce deterministically by
  `(epoch desc, content_hash asc)` and surface the loser as `Superseded` instead
  of requiring operator choice.

### 4. E2E Deploy Contract

Files:

- `MVP/e2e/Cargo.toml`
- `MVP/e2e/src/deploy_commit_drain_contract.rs`
- `MVP/e2e/src/main.rs`
- possible helpers in `MVP/e2e/src/bus_syntax.rs`

Scenario:

1. Create an island and sessions for operator, scheduler, nodes, projection,
   gateway/DNS writers, and an unauthorized principal.
2. Register two scheduler queue subscribers for `deploy.submit`; prove one
   accepts.
3. Register node capacity responders and participant RPC responders.
4. Submit a manifest with phase 1 DB and phase 2 web + queue.
5. Run capacity fanout and record visible nodes at decision time.
6. Start DB, verify readiness, and write phase 1 commit.
7. Start web + queue; verify neither gateway nor DNS projection routes the new
   backends before both are ready.
8. Write route, gateway, and DNS commit facts locally.
9. Run projection and verify snapshots contain the new serving state.
10. After `ServingCommit`, send drain requests for old backends.
11. Keep old backends alive through drain grace.
12. Run projection before old-instance stop and verify snapshots contain new
    active backends plus old-backend drain metadata.
13. Stop old backends after drain and report `DeployDone`.
14. Run a cleanup-failure variant where old stop fails after serving commit and
    verify `CleanupPending`.
15. Run an irreversible-phase failure variant and verify
    `DeployBlockedAfterIrreversiblePhase`.
16. Run a concurrent-serving-commit variant and verify deterministic
    supersession.

Metrics:

- scheduler queue deliveries,
- capacity fanout duration,
- visible nodes at decision time,
- phase 1 duration,
- phase 2 duration,
- local route commit duration,
- route commit to projection duration,
- drain duration,
- cleanup-pending count,
- old-backend alive checks during drain grace,
- elapsed scenario duration.

### 5. Slice Report And Decision Ledger

Files:

- `MVP/slice-010-deploy-commit-drain.md`
- `MVP/e2e-proof-plan.md`
- `MVP/primitive-decisions.md`

Report requirements:

- Record crate decisions from the crate scout.
- Record the old-code LOC baseline:
  `crates/ployzd/src/daemon/handlers/deploy.rs` and
  `crates/ployz-orchestrator/src/deploy/`.
- Compare semantic leverage in prose, not only LOC.
- Include a parity matrix with rows for each old deploy semantic covered,
  replaced, or deliberately deferred by this slice.
- Update E2E-6 current proof status.
- Add a "Changed Since Last Slice" entry for any deploy primitive that survives
  review.
- Call out that active-member/partition evidence remains deferred and must not
  become hidden quorum.

## Sequencing

1. Add the `mvp-deploy` domain crate and unit tests before touching E2E.
2. Add coordinator/participant bus adapters against `BusActorHandle`.
3. Connect serving-state commit to existing projection facts.
4. Add deterministic serving-commit conflict handling if current projection
   status cannot surface supersession.
5. Add `deploy-commit-drain-contract` to `mvp-e2e`.
6. Run a simplify pass before broadening the E2E scenario.
7. Run code review subagents against the slice.
8. Address review findings, then run a second simplify pass if the coordinator
   or state machine grew too much.
9. Commit implementation and simplify changes separately when practical.
10. Push the branch so PR #188 updates.

## Verification Plan

Targeted checks:

```text
cd MVP && cargo check -p mvp-deploy -p mvp-e2e
cd MVP && cargo test -p mvp-deploy
cd MVP && cargo run -p mvp-e2e -- deploy-commit-drain-contract
```

Full MVP checks:

```text
cd MVP && cargo clippy --all-targets -- -D warnings
cd MVP && cargo test --all
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

Repo-level smoke before push:

```text
just test
```

The `all` E2E budget stays time-boxed. If `deploy-commit-drain-contract`
pushes the aggregate run over budget, the slice must either optimize the
scenario or raise the budget explicitly in the plan/report with measured
justification.

## Review And Simplification Gates

Run review with subagents before finalizing the slice:

- correctness: deploy transition invariants and failure branches,
- testing: E2E coverage and typed error assertions,
- maintainability: state ownership, naming, and API size,
- reliability: no indefinite waits, no hidden reconcilers, no drain-before-commit
  path,
- performance: fanout/projection/drain timing and E2E budget impact.

Run simplify regularly:

- after the domain state machine passes unit tests,
- after the E2E scenario first passes,
- after review fixes.

Simplification should specifically look for:

- duplicate deploy status representations,
- boolean phase flags that should be enum variants,
- stringly IDs where newtypes should exist,
- coordinator methods that mix planning, fact writes, participant RPC, and
  presentation,
- test helpers that are harder to understand than the product behavior.

## Risks

- Recreating the old deploy handler under a new crate name. Keep the first
  canary small and typed.
- Letting the E2E harness become the product model. Participant simulation must
  sit behind product-shaped commands.
- Treating projection reload as authority. Durable facts are authority;
  projection is serving evidence.
- Accidentally turning visible nodes into quorum. Visible nodes belong in the
  command result and preflight diagnostics, not the commit path.
- Overusing advisory leases. Deploy ownership can use leases later if the
  product needs that explicit command-entry conflict surface; this slice should
  not introduce them by default.
- Hiding cleanup failures after commit. Cleanup failure must be status with an
  audience.

## Definition Of Done

- `mvp-deploy` exposes a small typed API that future business logic can use
  without knowing bus internals.
- Unit tests prove invalid state transitions are impossible through public
  methods or fail with structured variants.
- `deploy-commit-drain-contract` proves route/gateway/DNS facts are written
  before drain begins and projected before old-instance stop.
- The E2E scenario proves old backends stay alive during drain grace.
- Failure after irreversible phase and cleanup failure after `ServingCommit` have
  separate typed outcomes.
- Command results include visible nodes at decision time.
- No new code outside `MVP/`.
- The slice report and decision ledger explain any new primitive and what it
  replaces.
- Review and simplify passes have been run, and their findings have either been
  fixed or explicitly recorded.
