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
- p2panda signed operations and local stores are the preferred durable fact
  substrate behind `FactSource`.
- iroh-blobs carries content-addressed payloads.
- SQLite is a disposable local projection/cache.
- WireGuard remains the private data plane.
- HTTP gateway and DNS behavior remain product requirements, but their internal
  shape is open for redesign. Pingora and the existing DNS code are references,
  not constraints.
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

Simplicity is a hard requirement, not a nice-to-have. The target is the simplest
code that preserves the required semantics: easy to read, easy to maintain,
easy to test, and hard to misuse from future business logic. A slice that
passes E2E tests but forces feature code to understand transport, timing,
authorization, or storage choreography has not proved the foundation.

The strategy is to rebuild the foundation by proof. Every future slice should
answer: what is the next smallest proof that makes the architecture more real?

## Product Constraints

The source of truth remains [VISION.md](../VISION.md).

This MVP is a proposed amendment to one part of the current vision: NATS stays
as the semantic model for subjects, requests, queues, services, permissions,
and authority boundaries, iroh remains the candidate connectivity substrate,
and p2panda is now the preferred durable fact substrate. That decision should
be proven here before replacing the existing NATS path.

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
- The operator's connected node is the consistency boundary for a command. A
  command writes durably to that node's local fact store, returns, and lets
  replication converge eventually.
- Command results must include the visible nodes at decision time, so operators
  see the reachability context the command used instead of the system blocking
  on a quorum.
- A future active-member or partition-view primitive may improve those
  reachability checks. That is intentionally pushed out of the MVP commit
  boundary; it must be added as explicit decision-time evidence, not as hidden
  quorum behavior.
- The daemon is disposable. Workloads, WireGuard, HTTP serving, DNS serving,
  and last-good data-plane state must outlive it.
- If a command cannot prove preconditions, it fails before mutation.
- If a command crosses a durable commit point, later cleanup failure is visible
  recoverable status, not erased history.
- The coordinator daemon is not the data plane. Killing the command/coordinator
  role must stop new local mutations and operator commands, but steady-state
  roles continue serving: workloads keep running, WireGuard remains configured,
  service-to-service traffic still works, HTTP/DNS keep serving last good state,
  and local appliers can continue consuming already-replicated serving-state
  updates if their role is not the crashed coordinator.

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
State           = signed p2panda fact operations behind FactSource
Projection      = SQLite plus snapshots
Runtime         = Kameo actors
Connectivity    = iroh first, WireGuard data plane
HTTP serving    = product primitive; Pingora is a candidate implementation
DNS serving     = product primitive; role/process shape must be proven
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
- facts write durably on the connected node and replicate eventually,
- projections rebuild,
- HTTP/DNS serving keeps last good data-plane state,
- machine add/remove works through iroh and WireGuard reconciliation,
- deploy commit happens before drain,
- crash/restart behavior preserves steady-state data-plane behavior when the
  coordinator is down,
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

## Current Code To Study, Not Preserve

Use these as reference material, but do not treat their current shape as a
non-negotiable migration target:

- Pingora HTTP serving implementation and snapshot state patterns in
  [crates/ployz-gateway](../crates/ployz-gateway)
- DNS serving behavior in [crates/ployz-dns](../crates/ployz-dns)
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
- ACME behavior in
  [crates/ployz-cert-backends](../crates/ployz-cert-backends) and
  [crates/ployzd/src/daemon/cert_coordination.rs](../crates/ployzd/src/daemon/cert_coordination.rs)

The old deploy implementation is specifically not a shape to preserve.
[crates/ployzd/src/daemon/deploy.rs](../crates/ployzd/src/daemon/deploy.rs)
is the semantic-leverage baseline to beat, not a porting source.

The HTTP/DNS rewrite should preserve product behavior and data-plane continuity,
not the old control-plane input model or role boundaries. Pingora may still be
the right HTTP serving primitive; what feeds it is up for redesign.

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
- HTTP/DNS serving paths that require live control-plane connectivity before
  serving last good state.
- Any deploy path where drain can start before the route commit is durable.
- The old deploy coordinator shape in
  [crates/ployzd/src/daemon/deploy.rs](../crates/ployzd/src/daemon/deploy.rs).
- Any architecture where one crashed coordinator process prevents already
  running services from communicating, serving HTTP/DNS, or consuming
  already-replicated local serving-state updates.

## Daemon Failure Semantics

For the MVP, "kill the daemon" means kill the role that accepts operator
commands and coordinates mutations. It must not mean the node's steady state is
dead.

Expected while the coordinator is down:

- existing workloads keep running,
- WireGuard configuration remains active and service-to-service traffic across
  nodes continues,
- HTTP/DNS serving continues from last good local state,
- fact-sync, projection, and snapshot applier roles keep applying
  already-replicated serving-state facts and publishing atomic gateway/DNS
  snapshots,
- the node reports coordinator health/staleness visibly, rather than silently
  claiming all control-plane capabilities are healthy.

Unavailable while the coordinator is down:

- new deploys or mutations targeted at that node,
- local runtime changes that need the coordinator to modify containers,
  firewall/WireGuard policy, routes, DNS, or certificates,
- operator commands that require fresh local precondition checks.

This distinction should drive future process-role design. We may still ship one
binary, but the coordinator, data-plane serving, workload runtime, and
state-applier responsibilities must not share a fate. Any exception has to be
called out as a lost MVP invariant before it is accepted.

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
- what crates or existing projects were checked before writing plumbing,
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
- what maintainer-facing documentation should be updated so future contributors
  know why a primitive, crate, or pattern exists.
- how the slice stays isolated under `MVP/`.
- how the slice handles p2panda persistence/sync and iroh connectivity instead
  of deferring the real substrate again.

The slice plan should not blindly follow a prewritten backlog. It should inspect
the code and choose the next boundary.

## Commit And Review Cadence

- Keep plan, implementation, simplification, and review-fix commits separate
  when a slice is larger than a narrow docs-only change.
- Run the simplify workflow after the first implementation proof passes, then
  land simplification as its own commit before the full review pass.
- Treat review-caught invariant bugs as a signal to reduce commit size inside
  the slice.
- Keep `just test` time-budgeted so the all-scenario E2E gate fails on
  meaningful wall-clock regressions instead of silently growing.

## Next Product Proofs

Before the next product-feature implementation slice, run the p2panda
substitution investigation in
[MVP/slice-018a-p2panda-substitution-investigation-plan.md](slice-018a-p2panda-substitution-investigation-plan.md).
The question is whether maintained p2panda crates can replace enough custom
fact/auth/sync/blob substrate to reduce the long-term maintenance burden before
the deploy restart recovery proof hardens those boundaries.

That investigation is complete. Slice 018b added the first p2panda-backed fact
substrate using `p2panda-core`, `p2panda-store`, and `p2panda-stream` behind the
existing projection-facing `FactSource` boundary.
See
[MVP/design-notes/p2panda-substitution.md](design-notes/p2panda-substitution.md).
The slice plan and report are
[MVP/slice-018b-p2panda-fact-substrate-plan.md](slice-018b-p2panda-fact-substrate-plan.md)
and
[MVP/slice-018b-p2panda-fact-substrate.md](slice-018b-p2panda-fact-substrate.md).

Slice 018c then moved deploy restart recovery onto that p2panda-backed fact
boundary. Deploy decision, serving commit, and cleanup-done facts now share one
p2panda-backed proof path; recovery exports the surviving operations, imports
them into a fresh fact store, and a fresh coordinator can recover pending
cleanup after the original coordinator dies post-serving-commit and before
drain. The slice report is
[MVP/slice-018c-p2panda-deploy-restart-recovery.md](slice-018c-p2panda-deploy-restart-recovery.md).

Slice 019a then ran the deep p2panda substitution audit:
[MVP/slice-019a-p2panda-substitution-audit-plan.md](slice-019a-p2panda-substitution-audit-plan.md).
The report is
[MVP/design-notes/p2panda-substitution-audit.md](design-notes/p2panda-substitution-audit.md).
The audit decision moved persistent p2panda fact storage ahead of ACME. Slice
019b completed that proof: p2panda operations now persist in SQLite, derived
Ployz indexes rebuild from the operation log, and a process-role serving proof
can project from the persistent p2panda store while preserving last-good
serving state.

Slice 020 then replaced manual operation copying as the main replication proof.
`mvp-p2panda-facts` now exposes a narrow `p2panda-sync` adapter: sync scopes
are checked against store-owned trusted author bindings, same-island replica
authority is explicit, received operations still pass through Ployz import
validation, and E2E proves persistent-store sync, projection rebuild,
conflicts, no-op repeat sync, payload-read grants, and 10,000-operation sync
stress.

Slice 021 moved ACME onto the p2panda fact boundary. The
`p2panda-acme-http01-contract` scenario writes advisory lease and HTTP-01
challenge facts through `mvp-p2panda-facts`, syncs them with the Slice 020
adapter, projects on a second local store, serves last-good HTTP-01 state while
the issuer/coordinator adapter is absent, rejects stale/scoped writes, and
rebuilds SQLite from synced p2panda operations.

Slice 022 then made the concrete p2panda-net decision. Current git
p2panda-net is useful now as the maintained iroh/gossip/log-sync carrier, and
it is acceptable to avoid direct rc iroh usage for this MVP path. It did not
replace the stable production `PandaFactStore` API line: current git p2panda
operation/store types are kept in a transport/quarantine role, with stable
Ployz fact-operation envelopes carried as payloads and imported through the
canonical authority path on receipt.

Slice 023 closed both immediate proof gaps:

1. Deploy pre-serving cleanup now has an explicit participant ABI. A reversible
   failure after prepare/start sends bounded candidate cleanup RPCs, returns a
   structured foreground cleanup report, never drains old backends before a
   serving commit, and recovers from decision/no-serving-commit facts without
   rerunning participant mutation.
   See
   [MVP/slice-023-deploy-candidate-cleanup-abi.md](slice-023-deploy-candidate-cleanup-abi.md).
2. p2panda-net now has owned-node transport coverage and a product canary.
   Stable Ployz fact envelopes travel over owned p2panda-net nodes, enter the
   canonical trusted-replica import path, and ACME HTTP-01 can serve from
   transported facts while the issuer adapter is absent.
   See
   [MVP/slice-023-owned-p2panda-net-transport.md](slice-023-owned-p2panda-net-transport.md).

Slice 024 paid down the ACME side of that deletion-backed direction inside the
MVP boundary. `mvp-acme-command` now owns claim, present, clear, lease replay,
preflight, visible-node evidence, and structured errors. The p2panda ACME E2E
keeps transport/projection/serving proof but no longer owns the command state
machine. See
[MVP/slice-024-acme-command-surface.md](slice-024-acme-command-surface.md).

Slice 025 then consolidated p2panda-net behind `mvp-p2panda-transport`. Product
E2Es no longer import git p2panda network/store/sync APIs directly, ACME lost
its local p2panda-net harness, test wire helpers are feature-gated under the
transport crate's harness module, and the obsolete `mvp-p2panda-spike` crate
was deleted after `mvp-p2panda-facts` covered its proof value. See
[MVP/slice-025-p2panda-net-substitution-consolidation.md](slice-025-p2panda-net-substitution-consolidation.md).

Slice 026 then did the deploy command-surface equivalent of Slice 024 without
porting old `deploy.rs`: `mvp-deploy-p2panda` now owns the reusable p2panda
fact writer and recovery-read adapter, while `mvp-deploy` remains core-only.
The deploy restart proof still expresses the smallest durable state machine
from `MVP/architecture.md`: request-many capacity, prepare/start,
p2panda-backed serving commit, projection catch-up, then drain as a
consequence of that commit. See
[MVP/slice-026-deploy-p2panda-command-surface.md](slice-026-deploy-p2panda-command-surface.md).

Slice 027 adds the volume movement canary:
[MVP/slice-027-volume-ownership-transfer.md](slice-027-volume-ownership-transfer.md).
`volume-transfer-contract` proves advisory lease fencing, bus-backed
snapshot/receive, exact receive-evidence validation, p2panda-backed ownership
commit, post-commit recovery, stale-holder rejection, and pre-commit no-success
semantics without adding ZFS, a generic workflow runner, or a reusable volume
p2panda adapter.

The routing-owned serving commit correction then moved serving fact writer
ownership to `mvp-routing`. Deploy and machine remove both consume the same
`ServingFactWriter` contract, and the p2panda serving writer lives in
`mvp-routing-p2panda` rather than deploy. See
[MVP/slice-027-routing-owned-serving-commit.md](slice-027-routing-owned-serving-commit.md).

Slice 028 completes that p2panda machine-remove canary:
[MVP/slice-028-p2panda-machine-remove-facts.md](slice-028-p2panda-machine-remove-facts.md).
`machine-remove-contract` now projects joined-node facts, removal/tombstone
facts, and serving commits from one p2panda-backed fact source. The slice adds
`mvp-machine-p2panda`, deletes the E2E-local iroh-docs machine writer and
combined fact source, proves scoped join/machine/routing write authority, and
keeps coordinator-resume after serving commit explicitly deferred.

Slice 029 closes that deferred recovery point:
[MVP/slice-029-machine-remove-restart-recovery.md](slice-029-machine-remove-restart-recovery.md).
Machine remove now writes a command decision fact after target probe and before
mutation, writes cleanup-done only after tombstone, and can recover pending
cleanup from p2panda-backed facts after the original coordinator and in-memory
pending value are gone. The same slice also centralizes repeated p2panda store
wrapper mechanics in `SharedPandaFactStore`, so deploy, routing, machine, and
volume fixtures share the same storage shell while domain writers keep their
own error semantics.

Slice 030 upgrades the p2panda-net proof shape:
[MVP/slice-030-p2panda-net-fact-node.md](slice-030-p2panda-net-fact-node.md).
The transport crate now exposes a running fact-node wrapper: p2panda-net owns
the iroh/gossip/log-sync carrier, while `SharedPandaFactStore` owns Ployz
authorization, trusted replica import, conflict candidates, and projection
reads. The E2E projects from the receiver's synced local store instead of
collecting network bodies and manually importing them in the scenario. The
non-RC-iroh decision remains isolated inside `mvp-p2panda-transport`; domain
crates do not learn git p2panda transport types.

The next implementation/proof slice should keep paying down product semantic
leverage rather than adding another generic substrate layer. Plan it against
the current map and prefer a product rule that reuses bus, p2panda facts,
projection, advisory leases, serving actors, or deploy adapters without
growing those foundations again.

Future multi-phase commands should watch for the pattern documented in
[MVP/design-notes/phased-command.md](design-notes/phased-command.md). Do not
add the primitive prematurely. When three or more commands have phase enums,
resume-from-phase logic, and non-trivial compensation, plan a dedicated
`mvp-commands` slice that lifts the bookkeeping while keeping step and
compensation logic explicit.

## Crate Scout Protocol

Before each implementation slice, do a short dependency scout and record it in
the slice plan.

The scout should answer:

- What plumbing would this slice otherwise need to build?
- Which crates or adjacent projects already solve that plumbing?
- Are they maintained enough and compatible with the MVP architecture?
- What should be adopted now, what should be deferred, and what ideas should be
  copied without adding a dependency?

The default is to lean on well-tested crates for substrate plumbing and keep
Ployz-specific business semantics in our own code. Good candidates to check as
they become relevant include iroh protocol helpers such as `irpc`, Kameo,
`async-nats` as a semantic reference, `cedar-policy` or `biscuit-auth` for
authorization, `tokio-util` for cancellation/shutdown, `rusqlite` for
projections, and load/stress tooling such as `criterion` when a slice needs
measurement.

## Maintainer Documentation Protocol

The MVP should build a small set of maintainer-facing architecture notes while
the system is still easy to explain. The goal is not public marketing docs; it
is to make the chosen “Lego pieces” legible to a future maintainer who needs to
understand why the code is shaped this way.

Use [MVP/primitive-decisions.md](primitive-decisions.md) as the current decision
map. Update it when a slice:

- adopts or rejects a crate for substrate plumbing,
- introduces a new primitive,
- changes the public semantics of a primitive,
- discovers a cost or failure mode future maintainers should remember,
- proves that a simpler approach is enough and a heavier dependency is not
  justified yet.

If a decision is too speculative to commit to the primitive map, record it as a
future documentation problem in the slice report instead. Documentation should
track decisions that have evidence, not every idea raised during exploration.

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
- wiring daemon/serving-role boundaries.

The simplification goal is not fewer lines. It is clearer ownership and fewer
ways to represent the same state. Prefer typed variants, narrow traits, and
deterministic reducers over stringly option bags or broad facades.

## E2E Gate Categories

Each future slice should improve at least one of these gates:

- `Bus`: subject matching, request/reply, no responders, request-many, queue
  groups, drain, and auth failures.
- `Authority`: island membership, grants, imports/exports, direct fact-write
  denial across islands.
- `Facts`: signed immutable facts, local durable writes, eventual replication,
  conflict candidates, deterministic reducer supersession, and projection
  rebuild.
- `Leases`: advisory TTL, renewal, epoch fencing, RAII release, and loud
  conflict surfaces without quorum or witness-ack collection.
- `HTTP/DNS`: last-good serving, corrupt next-state handling, daemon outage,
  and whatever role/process boundary the new serving design proves.
- `Membership`: init, invite, join, tombstone, WireGuard full mesh.
- `Deploy`: capacity fanout, phase readiness, durable commit, route projection,
  drain, crash before/after commit.
- `Scale`: publish fanout, request-many aggregation, projection lag, memory per
  logical node, p99 latency, including 1,000 and 10,000 logical-node stress
  gates.
- `Semantic leverage`: reimplement representative old-codebase features and
  compare feature-code clarity, test shape, and amount of substrate glue.
- `Simplicity`: review whether the implementation is easy to understand,
  whether concepts have one representation, whether feature authors get a small
  ergonomic API, and whether complexity is isolated behind primitives rather
  than leaked into business logic.

Recent semantic-leverage proof:

- Slice 026 extracts deploy p2panda fact-writing/recovery glue from the
  restart-recovery E2E into `mvp-deploy-p2panda`. The deploy coordinator stays
  core-only, while the p2panda adapter becomes reusable business plumbing for
  decision, serving commit, cleanup-done, and recovery reads.
- The first LOC check is directionally good but not a free pass. Representative
  deploy code is materially smaller than the old deploy surface, but the MVP
  foundation has a real upfront cost. New slices should report whether they add
  product logic on top of existing primitives or grow bus/fact/projection
  substrate again.

## Non-Goals Until Proven Necessary

Defer these until an E2E proof or product requirement makes them necessary:

- JetStream-like persistence.
- Global total ordering.
- Distributed SQL.
- Hard consensus.
- Quorum/witness-ack collection for fact commits or leases.
- Automatic partial WireGuard graph selection beyond full mesh.
- Optimized wildcard subject indexes for very large fleets.
- Full adversarial multi-tenant hosting.
- Automatic rollback for irreversible phases.
- Replacing or keeping Pingora before an HTTP-serving slice proves the right
  shape.
- Removing the existing NATS path before the MVP path has proof.

## Open Strategy Questions

Future slice plans should resolve these only when they become blocking:

- Whether the first implementation should integrate into existing `ployzd` or
  run as a parallel MVP daemon path until the proof harness passes. Current
  direction: keep it under `MVP/` until explicitly changed.
- Which current p2panda-store APIs are stable enough for the canonical MVP fact
  store. Slice 022 answered the transport side: p2panda-net can carry fact
  envelopes now, but the current git store/operation API line should not replace
  the stable production `PandaFactStore` path until the authority/import seam is
  narrower.
- Whether Kameo remote actors should be avoided entirely at first, keeping all
  remote semantics in PloyzBus.
- Whether `ployz-store-api` should evolve into a projection-facing interface or
  be bypassed for the MVP path.
- Which serving-state encoding is best for HTTP/DNS. Start readable unless
  tests prove it is too slow.
- Whether a future active-member or partition-view primitive should let
  commands check currently alive members before mutation. This may become useful
  evidence, but it should not be smuggled back in as quorum-style commit
  semantics.
- What explicit reinvite/clear primitive should exist for a tombstoned node id.
  The current MVP join path treats tombstones as durable exclusion and does not
  allow a normal higher-epoch join fact to resurrect the node.

## First Next Step

The next action after this strategy map is committed should be a fresh planning
pass for the first implementation slice.

The first slice should probably target the smallest end-to-end proof of
PloyzBus semantics in memory, because that gives later iroh, actor, and E2E
work a stable semantic contract. But that is a hypothesis for the next
`ce-plan` pass to validate against the current codebase, not a fixed slice
declared by this document.
