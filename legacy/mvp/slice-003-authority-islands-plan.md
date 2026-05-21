---
title: Slice 003 Authority Islands Plan
status: active
created: 2026-05-17
origin:
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
---

# Slice 003 Authority Islands Plan

## Problem Frame

The bus now proves NATS-shaped local semantics, bounded delivery, actor-facing
access, and large logical-node fanout. The next missing foundation is authority
isolation. Right now a `BusSession` carries a principal and a grant, but it does
not carry an island boundary, and the MVP has no fact-write authorization
surface. That means the current proof cannot yet demonstrate the central
island claim from `MVP/architecture.md`: subjects are island-local, transport
identity is not authority, and laptop/prod truth is not accidentally merged.

This slice should prove single-island and multi-island isolation in memory,
without implementing bridge import/export or iroh-docs replication yet. The
single proof target is:

> Two authority islands can use the same subject names and fact keys without
> seeing or mutating each other's truth, while grants authorize subject and fact
> operations before dispatch or durable mutation.

## Why This Is Next

This is the smallest slice that moves beyond bus mechanics toward the actual
Ployz control-plane architecture. Bridge rules, service registry facts,
iroh-docs facts, machine membership, and deploy commits all depend on the same
ownership rule: an operation happens inside one island unless an explicit
import/export or grant says otherwise.

Starting with iroh transport or iroh-docs before this boundary would make the
hard part look like networking. The harder product rule is authority: which
principal may publish, subscribe, respond, drain, or write facts in which
island.

## Scope

Implement an MVP-local authority model that:

- makes `BusSession` island-scoped,
- makes bus messages island-scoped,
- keeps subject matching and queue selection island-local,
- supports subject publish/subscribe/queue/respond/drain grants per island,
- supports fact-write grants and denies for fact key patterns,
- supports explicit grant revocation,
- records structured authorization failures,
- provides an in-memory authorized fact set for this slice's proof,
- exposes future bridge/import/export types as data models only if needed by
  tests, without forwarding cross-island traffic yet.

Out of scope for this slice:

- bridge import/export forwarding,
- iroh, iroh-gossip, iroh-docs, or iroh-blobs integration,
- fact replication or pin quorum,
- SQLite projection,
- service registry facts,
- machine join/remove,
- deploy state machines,
- gateway/DNS process roles.

## Current Patterns To Preserve

- Keep normal business-facing bus access through `BusActorHandle`.
- Keep raw synchronous bus access under `mvp_bus::harness::InMemoryBus` for
  contract and scale proof only.
- Keep reply permits one-use and deadline-bound.
- Keep authorization failures structured; do not rely on display-string parsing.
- Keep tests product-shaped: "laptop cannot write prod facts" is better than
  "grant array index 2 is false."
- Keep all code, docs, and tests under `MVP/`.

## Crate Scout

The slice would otherwise need a small authorization model, subject/fact pattern
matching, and testable grant evaluation. Checked options:

- `cedar-policy` 4.10.0: strong Rust policy engine for principal/action/resource
  authorization. It is appropriate when policies need an external policy
  language, schemas, diagnostics, and non-trivial resource/entity relationships.
  Defer it for this slice because the MVP needs a tiny, inspectable authority
  surface whose behavior is part of the product semantics, not a generic policy
  DSL.
- `biscuit-auth` 6.0.0: decentralized authorization tokens with offline
  attenuation and public-key validation. Defer it because this slice needs
  island-owned grants and revocation semantics; Biscuit's strengths are bearer
  capabilities and delegated tokens, and revocation still needs external state.
- NATS account import/export docs: use as the semantic reference for island
  isolation, streams versus services, and subject remapping. Do not add NATS as
  a dependency.
- Existing `Subject`/`SubjectPattern`: reuse the current NATS-like pattern
  matcher for bus subjects. For fact keys, prefer a small typed `FactKey` and
  `FactKeyPattern` rather than overloading bus subjects if slash-style fact keys
  make reducer docs easier to read.

Decision for this slice: implement a small MVP-local authority evaluator and
record the decision in `MVP/primitive-decisions.md`. Revisit Cedar/Biscuit only
when bridge tokens, delegated invites, or operator-editable policy become real
requirements.

Sources:

- `cedar-policy` docs describe Cedar as a policy language and authorization
  evaluator for application permissions:
  <https://docs.rs/cedar-policy/latest/cedar_policy/>
- Biscuit docs describe decentralized validation, offline delegation, and note
  that revocation needs external state:
  <https://docs.rs/biscuit-auth/latest/biscuit_auth/>
- NATS accounts isolate message visibility and use exports/imports for
  cross-account streams/services:
  <https://docs.nats.io/running-a-nats-service/configuration/securing_nats/accounts>

## Implementation Units

### U1: Authority Model

Goal: introduce explicit island identity and grants without widening the public
bus API.

Files:

- Modify: `MVP/bus/src/grants.rs`
- Modify: `MVP/bus/src/message.rs`
- Modify: `MVP/bus/src/error.rs`
- Modify: `MVP/bus/src/lib.rs`
- Test: `MVP/bus/src/grants.rs`
- Test: `MVP/bus/src/memory.rs`

Approach:

- Add typed `IslandId`.
- Make `BusSession` carry `IslandId` plus `PrincipalId`.
- Make `BusMessage` carry `IslandId`.
- Add `GrantBook` storage keyed by `(IslandId, PrincipalId)`.
- Add grant constructors that make the intended island explicit.
- Keep grant check methods crate-private.
- Keep existing subject authorization behavior within an island.
- Update structured errors to include island context where useful.

Execution note: test-first for isolation and revocation behavior. Existing bus
contract tests should keep passing after updating setup helpers to create a
default island.

Test scenarios:

- A publisher and subscriber in the same island communicate normally.
- A publisher in island A cannot deliver to a subscriber in island B on the
  same subject.
- A request in island A does not see responders in island B and returns
  `NoResponders`.
- Revoking a principal's grant stops future publish/request before dispatch.
- Error variants expose enough typed context for callers to branch by island,
  principal, and operation.

### U2: Authorized Fact Set

Goal: prove fact-write authorization without taking on iroh-docs yet.

Files:

- Create: `MVP/bus/src/facts.rs` or `MVP/facts/src/lib.rs`
- Modify: `MVP/bus/src/lib.rs` or `MVP/Cargo.toml` if a new crate is justified
- Test: matching unit tests beside the implementation

Approach:

- Add `FactKey`, `FactKeyPattern`, `FactContentHash`, `Fact`, and
  `InMemoryFactSet` types.
- Facts are immutable for this slice: writing the same `(island, key)` with the
  same content hash is idempotent, and a different content hash is stored as a
  bounded conflicting candidate for projection.
- Fact writes require an island-scoped `fact_write_allow` match and no matching
  `fact_write_deny`.
- Reads are island-scoped.
- The fact set should be intentionally small and replaceable by iroh-docs in a
  later slice; it is the contract harness, not the final storage backend.

Execution note: keep this as pure domain/storage code, not an actor yet, unless
the implementation becomes stateful enough that actor ownership clarifies it.

Test scenarios:

- A principal with an allowed fact-write pattern can write an allowed fact.
- A principal without fact-write permission cannot write a fact.
- A deny pattern wins over allow.
- Same key and same hash is idempotent.
- Same key and different hash is returned as `FactWriteOutcome::Conflict` and
  listed as a conflicting candidate, not a write-time error.
- Reads from island B cannot see island A's facts.

### U3: Authority E2E Contract

Goal: add an MVP E2E scenario that proves authority islands at the product
semantic level.

Files:

- Create: `MVP/e2e/src/authority_contract.rs`
- Modify: `MVP/e2e/src/main.rs`
- Modify: `MVP/e2e/src/bus_syntax.rs` if fact-key helpers are useful
- Update: `MVP/README.md`
- Update: `MVP/slice-003-authority-islands.md`
- Update: `MVP/primitive-decisions.md`

Approach:

- Add `cargo run -p mvp-e2e -- authority-contract`.
- Include the authority scenario in `cargo run -p mvp-e2e -- all`.
- Write a structured metrics artifact under
  `MVP/target/mvp-e2e/authority-contract-metrics.json`.

Test scenarios:

- `default` island publisher/subscriber communicate.
- `laptop` and `prod` use the same subject name without cross-delivery.
- Unauthorized publish fails before handler dispatch.
- Unauthorized request fails before handler dispatch or known responders in
  other islands are ignored.
- Temporary response permission remains scoped to one inbox and deadline.
- Authorized fact write succeeds.
- Unauthorized fact write fails.
- Laptop cannot write prod facts directly.
- Grant revocation blocks a later operation.

Metrics:

- authorized publishes,
- isolated publishes,
- denied publish attempts,
- denied fact writes,
- revocation failures,
- cross-island delivery count, which must be zero.

### U4: Scale And Simplicity Proof

Goal: keep the large-load proof honest after adding island checks, and document
whether business semantics stayed simple.

Files:

- Modify: `MVP/e2e/src/scale.rs`
- Update: `MVP/slice-003-authority-islands.md`

Approach:

- Keep existing 200, 1,000, and 10,000 logical-node scale cases.
- Add a small multi-island fanout case: many subscribers split across two
  islands, one publish in one island, and zero cross-island deliveries.
- Record whether adding an island-scoped business rule required touching only
  the authority/fact primitives and the E2E scenario, not transport internals.

Test scenarios:

- 1,000 logical subscribers split across two islands; publish in island A
  reaches only island A.
- Existing scale E2E still passes.
- Document line/file touch count for adding "laptop cannot write prod facts."

## Review Risks

- Security: authority bypass through `BusAuthority`, session construction, or
  raw harness access.
- Correctness: cross-island responder leakage through request/request-many.
- Maintainability: too many new authority concepts before bridge/facts need
  them.
- Performance: every dispatch now filters by island; the 10,000-node bus test
  should show the cost is still acceptable.
- API design: business logic should still use `BusActorHandle`, not raw grant
  checkers or storage internals.

## Verification

Targeted:

```text
cd MVP && cargo test -p mvp-bus authority
cd MVP && cargo run -p mvp-e2e -- authority-contract
cd MVP && cargo run -p mvp-e2e -- scale
```

Full MVP gate before commit/push:

```text
cd MVP && just test
git diff --check
git diff --name-only -- ':!MVP/**'
```

## Semantic-Leverage Check

This slice should make the business rule "laptop cannot write prod facts" look
like a small fact-write authorization test, not a tour through transport or
storage plumbing.

Record in `MVP/slice-003-authority-islands.md`:

- files touched to add the rule,
- whether the rule needed a new global enum variant,
- whether tests assert product behavior directly,
- what future feature authors should call instead of inspecting grants
  manually.

## Follow-Up Candidates

- Slice 004 should likely implement bridge import/export service and stream
  semantics, using this slice's island boundary.
- Slice 005 should likely replace the in-memory fact set with iroh-docs-backed
  replicated facts plus projection rebuild proof.
- Delegated invite tokens may justify Biscuit or a narrower signed-token helper
  later; do not decide that in this slice.
