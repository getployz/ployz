---
title: Iroh Bus MVP Strategy Map
status: active
created: 2026-05-17
origin:
  - VISION.md
  - docs/architecture.md
  - docs/authority-roadmap.md
  - docs/routing-and-deploys.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
---

# Iroh Bus MVP Strategy Map

## Purpose

This is the overall strategy map for rebuilding Ployz on a cleaner foundation.
It is not the slice plan.

Each implementation slice should run a fresh planning pass against this map and
pick its own boundaries based on the current code, the latest completed work,
and the next proof target. Do not treat this document as a pre-cut backlog.

The end state is a Ployz foundation where:

- Core NATS semantics are retained as product semantics.
- NATS server topology is no longer the foundation.
- iroh provides connectivity and protocol multiplexing.
- Kameo actors own local subsystem state and supervision.
- iroh-docs stores replicated durable facts.
- iroh-blobs carries content-addressed payloads.
- SQLite is a disposable local projection/cache.
- WireGuard remains the private data plane.
- Gateway and DNS remain separate serving roles and keep their existing shape
  where possible, especially Pingora in the gateway.
- The proof is a strong E2E suite, not just architectural plausibility.
- The code shape proves the primitives are right: real business behavior should
  require far less orchestration glue than the previous foundation.
- New implementation work stays isolated under `MVP/` until the MVP foundation
  has enough proof to justify migration into existing crates.

## Why This Exists

The current codebase has valuable pieces but the foundation has become hard to
reason about. The failure mode to avoid is continuing to accrete new state,
branches, and handler responsibilities onto paths that already mix transport,
authorization, orchestration, storage, and presentation.

The MVP should prove a smaller set of better primitives:

- subject-addressed communication,
- explicit authority islands,
- direct request/reply with clear no-responder behavior,
- load-balanced queue groups,
- service discovery,
- signed immutable facts,
- deterministic projections,
- command-shaped deploy and machine operations,
- data-plane survival during control-plane restart.

The other failure this MVP must address is semantic leverage. The previous
foundation grew to a very large amount of code with too little business logic in
proportion to substrate plumbing. That is a signal that the primitives were not
right. The new foundation should make a few real product features easier to
write, easier to test, and easier to review because the business behavior sits
on top of stronger bus, fact, projection, and actor primitives.

The strategy is to rebuild the foundation by proof. Every future slice should
answer: what is the next smallest proof that makes the architecture more real?

## Product Constraints

The source of truth remains [VISION.md](../VISION.md).

This MVP is a proposed amendment to one part of the current vision: NATS stays
as the semantic model for subjects, requests, queues, services, permissions,
and authority boundaries, but iroh/iroh-docs become the candidate deployed
substrate. That decision should be proven here before replacing the existing
NATS path.

Important constraints:

- Ployz targets small clusters an operator can reason about, not hyperscale
  fleets. The product target remains roughly 1-200 nodes.
- Larger logical simulations are required MVP stress tests. They prove the
  control-plane design has margin; they are not product positioning for a
  10,000-node real WireGuard topology.
- Mutating operations are foreground commands with an audience.
- No hidden controller should silently rewrite durable truth.
- Durable state records explicit operator intent and lifecycle facts.
- Live observation is checked at decision time and does not become stored truth.
- The daemon is disposable. Workloads, WireGuard, gateway, DNS, and last-good
  snapshots must outlive it.
- If a command cannot prove preconditions, it fails before mutation.
- If a command crosses a durable commit point, later cleanup failure is visible
  recoverable status, not erased history.

## Architecture North Star

The target architecture is described in [MVP/architecture.md](architecture.md).
The short version:

```text
AuthorityIsland = NATS Account semantics
PloyzBus        = NATS Core semantics
Bridge          = NATS Import/Export semantics
Grant           = NATS permissions plus fact/RPC permissions
RequestReply    = NATS inbox semantics over iroh streams
QueueGroup      = NATS queue group semantics
Service         = NATS service endpoint semantics
State           = signed iroh-docs facts
Projection      = SQLite plus snapshots
Runtime         = Kameo actors
Connectivity    = iroh first, WireGuard data plane
Gateway         = existing Pingora serving role
DNS             = existing serving role
```

The internal control-plane primitive should be PloyzBus, not raw gossip and not
raw iroh RPC. Public product primitives remain operator commands. Transport
details sit underneath bus semantics.

## Proof North Star

The target proof harness is described in
[MVP/e2e-proof-plan.md](e2e-proof-plan.md).

The MVP is not done when the types compile. It is done when E2E tests prove:

- bus semantics work,
- authority islands isolate truth,
- bridges import/export explicit subjects,
- facts replicate,
- projections rebuild,
- gateway/DNS keep serving last good state,
- machine add/remove works through iroh and WireGuard reconciliation,
- deploy commit happens before drain,
- crash/restart behavior preserves the data plane,
- performance is measured under the product target and under large logical-node
  stress loads.

It is also done only when implementation slices prove semantic leverage:

- a few real features from the old codebase are reimplemented on the new
  primitives,
- business logic is visible as small typed operations and reducers,
- transport/storage/retry/supervision glue is mostly hidden behind reusable
  primitives,
- tests describe product behavior rather than implementation choreography,
- code review can reason about feature rules without reading the whole
  substrate stack.

## Existing Code To Preserve

Preserve these unless a later slice plan proves a concrete reason to change
them:

- Pingora gateway implementation and snapshot state patterns in
  [crates/ployz-gateway](../crates/ployz-gateway)
- DNS binary and serving role in [crates/ployz-dns](../crates/ployz-dns)
- Sidecar detach/adopt semantics in
  [crates/ployzd/src/services/gateway.rs](../crates/ployzd/src/services/gateway.rs)
  and [crates/ployzd/src/services/dns.rs](../crates/ployzd/src/services/dns.rs)
- WireGuard backend mechanics under
  [crates/ployz-orchestrator/src/mesh](../crates/ployz-orchestrator/src/mesh)
  and
  [crates/ployz-runtime-backends/src/mesh](../crates/ployz-runtime-backends/src/mesh)
- Deploy commit/routing invariants from
  [docs/routing-and-deploys.md](../docs/routing-and-deploys.md)
- Stored intent/projection/live observation separation from
  [docs/authority-roadmap.md](../docs/authority-roadmap.md)

The gateway/DNS change should be about their control-plane input model: local
snapshots and projections first, not direct dependency on a live NATS store.

While the MVP is being proven, do not modify the existing codebase. Build new
experimental code under `MVP/` and use the old code only as reference material.
Migration into `crates/`, root workspace files, or existing docs should be a
separate explicit decision after the MVP evidence exists.

## Current Code To Challenge

Challenge these areas during future slice planning:

- Daemon handlers that mix transport, authorization, orchestration, storage, and
  rendering.
- Global request enums that grow whenever an internal node command is added.
- Store facades used where a subsystem needs one narrow fact or projection
  capability.
- Background loops that mutate durable control-plane truth.
- Projection or health state that is accidentally promoted into stored truth.
- Gateway/DNS startup paths that require live control-plane connectivity before
  serving last good state.
- Any deploy path where drain can start before the route commit is durable.

## Planning Protocol For Future Slices

Every implementation slice starts with a new `ce-plan` pass against this map.

The prompt shape should be:

```text
Use MVP/overall-plan.md, MVP/architecture.md, and MVP/e2e-proof-plan.md.
Plan the next implementation slice that most improves proof of the MVP
foundation. Do not assume the slice boundaries from a previous session.
Ground the plan in the current codebase and include E2E proof criteria.
```

The slice plan should decide:

- the single proof target for the slice,
- why that target is the next best step,
- what existing code should be reused,
- what new MVP-local crates/modules are justified,
- what must remain out of scope,
- the minimum E2E or simulation proof,
- targeted unit/integration tests,
- expected review risks,
- whether the slice should run behind feature flags or a parallel MVP path,
- what semantic-leverage metric the slice will inspect, such as lines of
  feature logic versus substrate glue, number of files touched to add a product
  rule, or clarity of business invariants in tests.
- how the slice stays isolated under `MVP/`.

The slice plan should not blindly follow a prewritten backlog. It should inspect
the code and choose the next boundary.

## Execution Protocol For Future Slices

After a slice plan exists:

1. Run `ce-work` on that slice plan.
2. Implement the slice in the smallest coherent change set.
3. Add proof tests as part of the slice, not after it.
4. Run targeted tests.
5. Run `ce-simplify-code` on the slice diff.
6. Run targeted tests again.
7. Run `ce-code-review` with subagents.
8. Address actionable review findings liberally.
9. Commit the slice.
10. Push the branch.

While the rewrite is isolated, run MVP-local checks from inside `MVP/` before
pushing. Repo-level `just` targets and full-workspace checks come later only as
part of an explicit migration into the existing codebase.

## Commit And Branch Protocol

Work on a dedicated branch. Commit regularly at proof boundaries:

- strategy/spec docs,
- planning artifact for a slice,
- implementation plus tests for a slice,
- review/simplification fixes when they are substantial.

Do not batch unrelated architectural moves into one commit. A future reviewer
should be able to identify which proof each commit advanced.

## Review Protocol

Review is mandatory for implementation slices.

Use subagents during review because this work crosses architecture,
reliability, testability, security/authorization, and maintainability. At
minimum, a slice touching code should be reviewed for:

- correctness,
- test coverage,
- maintainability,
- project standards,
- reliability/failure behavior,
- security/authorization when grants, facts, subjects, or transport are touched,
- performance when bus, projection, fanout, or hot serving paths are touched.

Address review comments before responding or moving to the next slice unless
the comment is clearly out of scope and should be recorded as follow-up.

## Simplification Protocol

Run simplification regularly, especially after:

- introducing a new crate,
- adding typed subject/fact models,
- adding actor messages,
- adding projection reducers,
- adding transport adapters,
- wiring daemon/gateway/DNS boundaries.

The simplification goal is not fewer lines. It is clearer ownership and fewer
ways to represent the same state. Prefer typed variants, narrow traits, and
deterministic reducers over stringly option bags or broad facades.

## E2E Gate Categories

Each future slice should improve at least one of these gates:

- `Bus`: subject matching, request/reply, no responders, request-many, queue
  groups, drain, auth failures.
- `Authority`: island membership, grants, imports/exports, direct fact-write
  denial across islands.
- `Facts`: signed immutable facts, replication, pin acknowledgements,
  projection rebuild.
- `Gateway/DNS`: snapshot load, reload, last-good serving, corrupt snapshot
  handling, daemon outage.
- `Membership`: init, invite, join, tombstone, WireGuard full mesh.
- `Deploy`: capacity fanout, phase readiness, durable commit, route projection,
  drain, crash before/after commit.
- `Scale`: publish fanout, request-many aggregation, projection lag, memory per
  logical node, p99 latency, including 1,000 and 10,000 logical-node stress
  gates.
- `Semantic leverage`: reimplement representative old-codebase features and
  compare feature-code clarity, test shape, and amount of substrate glue.

## Non-Goals Until Proven Necessary

Defer these until an E2E proof or product requirement makes them necessary:

- JetStream-like persistence.
- Global total ordering.
- Distributed SQL.
- Hard consensus.
- Automatic partial WireGuard graph selection beyond full mesh.
- Optimized wildcard subject indexes for very large fleets.
- Full adversarial multi-tenant hosting.
- Automatic rollback for irreversible phases.
- Replacing Pingora.
- Removing the existing NATS path before the MVP path has proof.

## Open Strategy Questions

Future slice plans should resolve these only when they become blocking:

- Whether the first implementation should integrate into existing `ployzd` or
  run as a parallel MVP daemon path until the proof harness passes. Current
  direction: keep it under `MVP/` until explicitly changed.
- Which current iroh-docs/iroh-sync APIs are stable enough for the repo's Rust
  toolchain.
- Whether Kameo remote actors should be avoided entirely at first, keeping all
  remote semantics in PloyzBus.
- Whether `ployz-store-api` should evolve into a projection-facing interface or
  be bypassed for the MVP path.
- Which snapshot encoding is best for gateway/DNS. Start readable unless tests
  prove it is too slow.

## First Next Step

The next action after this strategy map is committed should be a fresh planning
pass for the first implementation slice.

The first slice should probably target the smallest end-to-end proof of
PloyzBus semantics in memory, because that gives later iroh, actor, and E2E
work a stable semantic contract. But that is a hypothesis for the next
`ce-plan` pass to validate against the current codebase, not a fixed slice
declared by this document.
