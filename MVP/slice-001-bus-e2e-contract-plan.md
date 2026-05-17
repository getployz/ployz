---
title: "feat: MVP-local bus E2E contract"
type: feat
status: active
date: 2026-05-17
origin:
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
---

# feat: MVP-local bus E2E contract

## Summary

Create the first implementation slice entirely inside `MVP/`: a PloyzBus
semantic contract, an in-process implementation, and an MVP-local E2E harness
that proves the bus behaves like the internal control-plane primitive described
in the strategy map.

This slice must not modify the existing codebase, workspace, `Cargo.toml`,
`justfile`, `crates/`, `docs/`, or current E2E runner. It establishes a
self-contained foundation we can stress and iterate on before deciding what to
merge back into the main codebase.

## Problem Frame

The MVP strategy says NATS remains the semantic model, but not the deployed
server topology. Before adding iroh, actors, docs, gateway/DNS compatibility,
or deploy flows, `MVP/` needs a small typed bus contract that captures the Core
NATS semantics Ployz actually wants:

- subjects and wildcard matching,
- publish/subscribe fanout,
- request/reply inboxes,
- no-responder errors,
- timeout errors,
- request-many aggregation,
- explicit request targets for concrete subjects versus wildcard fanout,
- queue groups,
- drain,
- basic subject and response authorization.

Because the existing codebase must stay untouched for this phase, the first
proof should be an MVP-local Rust project and E2E harness under `MVP/`.

## Success Criteria

- `MVP/` contains an isolated build/test target for the bus contract.
- The bus API is typed enough that feature code does not traffic in raw subject
  strings after parsing.
- The MVP-local E2E harness asserts behavior, not logs.
- The first E2E scenario proves publish, request, request-many, queue groups,
  no responders, timeout, authorization failures, and drain.
- The slice records a semantic-leverage note: what product behavior became
  clearer, what glue disappeared, and what still feels too ceremonial.
- No files outside `MVP/` change in this slice.

## Scope

In scope:

- New MVP-local Rust project files under `MVP/`.
- Subject and subject-pattern parsing/matching.
- Message, header, inbox, request, response, queue, drain, and error types.
- Minimal grant model for publish/subscribe/respond authorization.
- In-process async bus implementation for the first E2E contract.
- MVP-local E2E scenario runner.
- MVP-local test command documentation.
- Unit tests for parsing/matching and bus behavior.
- E2E-style test that runs through the MVP-local harness.

Out of scope:

- Modifying root `Cargo.toml`.
- Modifying `justfile`.
- Modifying `crates/ployz-e2e`.
- Modifying any existing `crates/` code.
- iroh transport.
- iroh-docs or iroh-blobs integration.
- Kameo actor supervision.
- Authority bridge import/export.
- Gateway/DNS snapshot migration.
- Deploy coordinator behavior.
- Replacing existing NATS-backed code paths.

## Key Decisions

## Crate Scout

Checked before implementation:

- `async-nats`: useful semantic reference for subjects, request/reply, queue
  groups, and services. Not adopted because this slice is proving the internal
  PloyzBus contract without a NATS server topology.
- `nuid`: candidate for future production inbox IDs. Not adopted because this
  in-process contract can use monotonic message ids until transport and
  cross-process uniqueness matter.
- `matchit` and route-trie style crates: not adopted because NATS subject
  wildcards have different `*`/`>` semantics and this slice needs a tiny,
  auditable matcher with explicit tests.
- `tokio`/`tokio-util`: likely useful for later actor/transport slices. Not
  adopted here because the first proof is synchronous and in-process; adding an
  async runtime now would obscure the bus semantics this slice is testing.
- `cedar-policy` and `biscuit-auth`: strong candidates for authority-island
  planning. Deferred because this slice only needs a small grant model to prove
  routing, response, queue, and drain authorization boundaries.

Decision: implement the tiny in-memory substrate locally for slice 001, while
recording the semantic ideas to keep from NATS and the crates to revisit when
the slice reaches transport, authority islands, and production-grade id
generation.

Simplicity rule for this slice: the bus implementation may contain routing and
authorization plumbing, but future feature code should only see subjects,
typed targets, handlers, replies, and structured errors.

1. The first implementation lives completely under `MVP/`.
   This keeps the experimental foundation isolated and allows aggressive
   iteration without adding churn to the existing codebase.

2. The proof is E2E-local, not simulator-first.
   It should execute a real scenario through an MVP-local harness. It can be
   fast and in-process, but it should be framed as an end-to-end contract for
   the new primitive, not a deterministic model of old behavior.

3. PloyzBus is internal.
   Public product primitives remain operator commands. The bus scenario is
   acceptable here because this slice proves the internal substrate contract;
   later slices should layer operator-visible flows on top.

4. Queue groups and request-many are separate primitives.
   Queue groups load-balance to one responder. Request-many fans out and
   aggregates replies. `request` targets a concrete subject; `request_many`
   accepts an explicit target enum so wildcard fanout is not smuggled into a
   concrete `Subject`.

5. No-responder and timeout are different errors.
   No responders means the bus can prove no eligible handler exists now.
   Timeout means a handler was eligible but did not answer in time.

6. Authorization is minimal but real.
   The MVP grant surface starts with publish, subscribe, response, and fact
   write permissions. This slice only needs publish/subscribe/respond.

7. Replies use one-use permits.
   A request handler receives a reply permit tied to request id, inbox,
   responder, and deadline. Unauthorized response tests should attempt to reply
   with an invalid, expired, or wrong-principal permit.

## Candidate File Layout

The implementer should choose the exact Rust layout during `ce-work`, but keep
it under `MVP/`. A likely shape:

```text
MVP/
  Cargo.toml
  bus/
    Cargo.toml
    src/
      lib.rs
      subject.rs
      message.rs
      grants.rs
      memory.rs
      error.rs
    tests/
      bus_semantics.rs
  e2e/
    Cargo.toml
    src/
      main.rs
      bus_contract.rs
  slice-001-bus-contract.md
```

If a simpler single-crate layout is enough, use it. The important boundary is
that all implementation code remains below `MVP/`.

## Implementation Units

### U1. Add MVP-local Rust project and typed subjects

Files:

- Create under `MVP/` only.

Approach:

- Add an isolated Cargo project/workspace under `MVP/`.
- Add `Subject` and `SubjectPattern` newtypes.
- Parse subjects into tokens once.
- Support NATS-shaped wildcards: `*` for one token and `>` for a terminal
  multi-token wildcard.
- Reject empty tokens, empty subjects, and non-terminal `>`.
- Keep display strings at the boundary.

Test scenarios:

- `node.alpha.status` matches `node.*.status`.
- `node.alpha.status` matches `node.>`.
- `node.alpha.status` does not match `node.*`.
- Empty subject and malformed wildcard patterns return structured parse errors.
- Parsed subjects preserve their original display form.

Verification:

- Run the MVP-local subject tests from inside `MVP/`.

### U2. Add in-process bus semantics

Files:

- Create under `MVP/` only.

Approach:

- Define `BusMessage`, `Headers`, `Payload`, `ReplyInbox`,
  `RequestTarget`, `RequestManyPolicy`, `ReplyPermit`, and typed errors.
- Implement an in-process bus with:
  - `publish`,
  - `subscribe`,
  - `request`,
  - `request_many`,
  - `queue_subscribe`,
  - `drain`.
- Handlers should be async enough to model timeout behavior.
- Queue groups must select exactly one eligible handler per request/publish.
- Drain should reject new work and let already-started work complete or time out
  visibly.
- Keep grants small: publish allow, subscribe allow, response allow.

Test scenarios:

- Publish fans out to all matching normal subscribers.
- Queue publish delivers to one member of a queue group.
- Request receives one direct reply through an ephemeral inbox.
- Request returns `NoResponders` when no matching handler exists.
- Request returns `Timeout` when a matching handler does not reply before the
  deadline.
- Request-many aggregates multiple matching responders.
- Request-many to `RequestTarget::Pattern("node.*.capacity")` reaches both
  node capacity responders.
- Unauthorized publish fails before subscriber handler execution.
- Unauthorized subscribe fails before registration.
- Unauthorized response with an invalid/wrong-principal reply permit fails
  before requester receives a reply.
- Drain rejects new publish/request calls after it starts.

Verification:

- Run the MVP-local bus tests from inside `MVP/`.

### U3. Add MVP-local E2E bus contract runner

Files:

- Create under `MVP/` only.

Approach:

- Add a small executable or integration test that runs a bus contract scenario.
- The scenario should exercise the whole bus contract in one readable flow:
  - register two node capacity responders,
  - request-many `RequestTarget::Pattern("node.*.capacity")`,
  - register a scheduler queue group on `deploy.submit`,
  - submit two deploy requests and prove each is handled by exactly one
    scheduler,
  - prove `NoResponders`,
  - prove timeout,
  - prove unauthorized publish/response failures,
  - drain the bus and prove new work is rejected.
- Emit a small metrics artifact under `MVP/target/` or another ignored local
  output path.

Test scenarios:

- The MVP-local runner exits zero when the contract passes.
- The runner exits non-zero and names the failed semantic when an assertion
  fails.
- Metrics include request count, response count, no-responder count, timeout
  count, and queue deliveries.

Verification:

- Run the MVP-local E2E command from inside `MVP/`.

### U4. Add semantic-leverage note

Files:

- Create: `MVP/slice-001-bus-contract.md`

Approach:

- Record the slice's semantic-leverage result:
  - what product behavior the slice expresses,
  - where substrate glue is hidden,
  - what still feels too ceremonial,
  - what the next slice should simplify or stress.

Test scenarios:

- The note names at least one improvement and at least one remaining code-shape
  concern.

Verification:

- `git diff --check`

## Review Risks

- The bus can become a mini-framework if the first slice adds every future
  policy variant. Keep only the semantics needed by the E2E contract.
- An in-process bus can accidentally encode assumptions that do not survive
  iroh. Keep transport identity, endpoint id, and inbox fields explicit enough
  for the future adapter.
- Timeout tests can become flaky. Use deterministic local timing with generous
  bounds and assert typed timeout errors, not exact elapsed times.
- Authorization errors must happen before handler execution, otherwise tests
  will prove the wrong security boundary.
- The MVP-local workspace should not leak into the root workspace until the
  spec has earned migration.

## Execution Notes

- Use `ce-work` on this plan.
- Keep all implementation files under `MVP/`.
- Do not modify existing codebase files.
- Run `ce-simplify-code` after the bus implementation and before commit.
- Run `ce-code-review` with subagents before committing the implementation.
- Commit and push after the slice passes its MVP-local tests.
