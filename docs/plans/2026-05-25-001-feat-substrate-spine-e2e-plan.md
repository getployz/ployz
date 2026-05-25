---
title: "feat: Prove Substrate Spine End To End"
type: feat
status: completed
date: 2026-05-25
origin:
  - VISION.md
  - docs/architecture/ployz-1-0-roadmap.md
  - docs/architecture/functional-system-roadmap.md
  - docs/plans/2026-05-24-001-feat-corrosion-store-iroh-membership-plan.md
  - docs/plans/2026-05-24-003-feat-ployz-1-0-state-and-substrate-plan.md
  - docs/solutions/architecture-patterns/operator-perspective-commands-with-corrosion-rows-2026-05-24.md
---

# feat: Prove Substrate Spine End To End

## Summary

Finish the roadmap substrate proof before adding CLI or broader product
surface. The slice should prove that the primitives already landed on `main`
work together as one vertical path:

```text
iroh identity/RPC
  -> local Corrosion agent lifecycle
  -> membership schema
  -> Corrosion-backed MachineMembershipPort
  -> two nodes can join and observe the same machine rows
  -> restart keeps durable peer identity
```

This is a substrate validation PR, not another substrate expansion PR. Keep it
narrow: lifecycle harness, two-node membership e2e, restart identity proof, and
small documentation cleanup around row ownership and historical guidance.

## Requirements

- R1. Start local Corrosion for tests through a narrow lifecycle harness that
  allocates isolated state, exposes the API address, waits for readiness with a
  bounded deadline, and tears the process down cleanly.
- R2. Apply `crates/polis/src/membership/schema.sql` through existing Polis
  store primitives against the local Corrosion instance.
- R3. Exercise real `PeerRuntime` identity/RPC/probe primitives in the
  two-node slice. Do not use `FakePeerProbe` or `FakeMembership` for the new
  acceptance path.
- R4. Prove two nodes can join through
  `CorrosionMachineMembership`/`MachineMembershipService` and observe the
  resulting machine rows from both Corrosion-backed stores.
- R5. Prove restart identity: restarting a node with the same iroh identity
  path yields the same endpoint ID and can still observe the existing machine
  membership rows.
- R6. Bound every external wait: Corrosion startup/readiness, schema
  application, peer preflight, row query, and row visibility.
- R7. Keep Ployz product code free of Corrosion process details,
  `corro-client` types, and iroh transport internals outside existing adapter
  boundaries.
- R8. Add only docs/comments needed to clarify machine row ownership, epoch
  meaning, and which older fact-store/NATS guidance is historical.

## Non-Goals

- Do not build the Ployz CLI crate in this PR.
- Do not add mesh, namespace, WireGuard peer derivation, deploy, branch, or
  volume behavior.
- Do not migrate daemon startup fully to Corrosion. This PR may define the
  eventual boot order, but its required executable proof can live in test/e2e
  harness code.
- Do not build a generic process supervisor or generic distributed-store
  framework.
- Do not turn Corrosion rows into a command queue.

## Current Repo State

- `crates/polis/src/store.rs` has `CorrosionStore` client primitives for
  transaction, query, subscribe, updates, and schema application, but it does
  not start or own a Corrosion process.
- `crates/polis/src/membership/schema.sql` and
  `crates/polis/src/membership/schema.rs` define the machine row schema and
  statement helpers.
- `crates/ployz/src/composition.rs` has `PeerRuntime::start`,
  identity persistence, endpoint/ticket accessors, peer RPC listener startup,
  and `corrosion_machine_membership`.
- `crates/ployz/src/adapters/polis/machine_membership.rs` adapts
  Corrosion-backed rows plus a peer probe into `MachineMembershipPort`, but its
  tests use in-memory row/probe doubles.
- `crates/ployz-e2e/src/scenarios/machine_add.rs` proves the product service
  path with `FakeMembership`; it does not prove Corrosion, iroh, or two-node
  row visibility.
- CI runs `just test-all` in the PR workflow. HostExec e2e is a separate
  Docker/ZFS path and is not the right first place for this local substrate
  proof.

## Design Decisions

### Lifecycle Harness Scope

Add a local Corrosion lifecycle harness as test support around the existing
client primitives. It should own only process startup, temporary directories,
readiness detection, API address discovery/configuration, and shutdown.

The harness should not become the daemon lifecycle API yet. Product daemon
startup still needs a later design pass for boot ordering, health surfaces,
configuration, and operator-visible failure.

### Test Topology

Use the smallest topology that proves replication meaningfully:

- two `PeerRuntime` instances with separate durable identity paths;
- two local Corrosion agents/stores, unless Corrosion's local development mode
  strongly favors one agent with isolated clients for the first proof;
- membership schema applied through Polis;
- two `CorrosionMachineMembership` adapters;
- product-level `MachineMembershipService::add_machine` calls;
- bounded observation that both stores see the expected rows.

If the first implementation discovers that two local Corrosion agents require
additional peer bootstrap work, keep that in this PR only if it stays smaller
than the acceptance test. Otherwise land one-agent lifecycle plus a clearly
named ignored/todo two-agent test only after documenting the remaining
Corrosion bootstrap contract.

### Visibility Waits

Row visibility should use a deadline-driven helper that repeatedly queries or
consumes store updates until the expected typed row appears. Do not add sleeps
as synchronization. A failure should report which row was missing, which store
was queried, and which deadline expired.

### Restart Identity

The restart proof should be independent of row sync flakiness:

- start a `PeerRuntime` with an identity path;
- record its endpoint ID;
- shut it down cleanly;
- start a new `PeerRuntime` with the same path;
- assert the endpoint ID matches;
- use the restarted runtime in the membership slice or a focused companion
  test that observes the same Corrosion-backed rows.

### Documentation Cleanup

Update only narrow documentation:

- `crates/polis/src/membership/schema.sql` comments, if useful, to describe
  `epoch` as owner-issued machine row versioning for this slice.
- `docs/architecture/ployz-1-0-roadmap.md` or
  `docs/architecture/functional-system-roadmap.md` to mark this substrate
  proof as the current next step once implemented.
- Any older NATS/fact-store guidance that conflicts with the Corrosion row
  model should be marked historical/superseded, not rewritten wholesale.

## Implementation Units

### U1. Local Corrosion Lifecycle Test Support

Likely files:

- `crates/polis/src/store.rs`
- `crates/polis/src/test_support/` or `crates/polis/tests/`
- `crates/polis/Cargo.toml`

Work:

- Add a test-support-only lifecycle type that starts the local `corrosion`
  binary with an isolated temp directory and API address.
- Wait for readiness through `CorrosionStore::new` plus a small query or
  health-compatible request, with an explicit timeout.
- Kill and reap the process on drop/shutdown.
- Return structured setup errors for missing binaries, startup failure, bad
  address, readiness timeout, and shutdown failure.

Tests:

- lifecycle starts, creates a `CorrosionStore`, applies the membership schema,
  and shuts down cleanly;
- missing-binary/setup failure is reported as a setup error, not a panic.

### U2. Schema And Store Readiness Proof

Likely files:

- `crates/polis/src/membership/tests.rs`
- `crates/polis/src/membership/schema.rs`
- `crates/polis/src/store.rs`

Work:

- Reuse `membership_schema_statements()` and
  `CorrosionStore::apply_schema`.
- Add a focused integration test that inserts/upserts a machine row through
  existing statement helpers and reads it back from a real Corrosion-backed
  store.
- Keep raw SQL contained in Polis membership/store code.

Tests:

- real store accepts schema statements idempotently;
- upsert/readback covers the current `machines` columns, lifecycle value, and
  epoch value.

### U3. Two-Node Membership Vertical Slice

Likely files:

- `crates/ployz-e2e/src/scenarios/substrate_spine.rs`
- `crates/ployz-e2e/src/scenarios/mod.rs`
- `crates/ployz-e2e/Cargo.toml`
- `crates/ployz/src/composition.rs` if a small test-support seam is needed

Work:

- Create two durable temp identity paths and start two `PeerRuntime` instances.
- Start/connect Corrosion stores through the lifecycle harness.
- Apply the membership schema.
- Build `iroh_peer_rpc_probe` and `corrosion_machine_membership` adapters.
- Use real `MachineMembershipService::add_machine` to add/join a machine row.
- Assert both nodes observe the row through Corrosion-backed membership reads.

Tests:

- two-node substrate spine test passes without fakes;
- preflight failure remains structured if a peer runtime is not reachable;
- row visibility timeout includes the missing machine ID and observing node.

### U4. Restart Identity Acceptance

Likely files:

- `crates/ployz/src/composition.rs`
- `crates/ployz-e2e/src/scenarios/substrate_spine.rs`

Work:

- Add or extend a test that restarts one node with the same iroh identity path.
- Assert the endpoint ID is stable.
- Assert the restarted node can participate in the membership slice and observe
  the existing Corrosion-backed machine row.

Tests:

- same identity path produces the same endpoint ID after restart;
- different identity paths produce distinct endpoint IDs, if this is not
  already covered.

### U5. Docs Hygiene

Likely files:

- `docs/architecture/ployz-1-0-roadmap.md`
- `docs/architecture/functional-system-roadmap.md`
- `docs/architecture/ployz-rewrite.md`
- `docs/solutions/architecture-patterns/operator-perspective-commands-with-corrosion-rows-2026-05-24.md`
- `crates/polis/src/membership/schema.sql`

Work:

- Mark the substrate-spine e2e proof as the Milestone 0/Track B completion
  step once tests exist.
- Clarify that machine rows are owner-written product facts replicated through
  Corrosion.
- Clarify `epoch` for the current machine table as a machine-row version, not
  a global conflict clock.
- Mark conflicting old fact-store or NATS-centered control-plane guidance as
  historical where it can mislead implementation.

Tests:

- docs-only changes require normal formatting/lint/test checks through the
  overall PR validation, with no separate doc generator expected.

## Validation Plan

- Run focused Polis tests for store lifecycle, schema application, and real
  upsert/readback.
- Run focused Ployz tests for `PeerRuntime` identity persistence and peer RPC
  probe behavior.
- Run `cargo test -p ployz-e2e` for the substrate-spine scenario.
- Run `just test-all` before opening or marking the PR ready because this
  touches `ployz`, `polis`, and e2e behavior.

## Risks

| Risk | Mitigation |
| --- | --- |
| Local Corrosion agent flags/config differ across installed versions. | Keep process invocation isolated in one harness and fail with a typed setup error that prints the resolved binary and config path. |
| Two local Corrosion agents need bootstrap behavior not yet modeled. | First implement the lifecycle and one-store proof, then add the two-agent proof only with explicit peer bootstrap. If bootstrap exceeds the PR, leave a failing/ignored test plus documented contract for the next PR. |
| E2E becomes flaky because row visibility is timing-based. | Use deadline-driven query/update observation helpers and diagnostic failure output; do not rely on fixed sleeps. |
| Test support leaks into product APIs. | Keep lifecycle helpers behind test support or integration-test modules until daemon boot path design requires a product seam. |
| Documentation cleanup expands into roadmap rewrite. | Touch only conflicting lines that would mislead the substrate PR. |

## Open Questions For Implementation

- What exact `corrosion` CLI flags should the harness use for isolated local
  agents, API binding, and peer bootstrap?
- Should the first two-node proof use two Corrosion agents, or does one local
  agent plus two stores provide enough confidence before peer bootstrap lands?
- Does CI need to install a pinned `corrosion` binary for this PR, or should
  the real-agent integration tests be gated until the binary is available in
  the workflow?

## Handoff

Recommended branch:

```text
feat/substrate-spine-e2e
```

Recommended first implementation order:

1. U1 lifecycle harness.
2. U2 real schema/upsert/readback proof.
3. U3 two-node membership slice.
4. U4 restart identity acceptance.
5. U5 docs hygiene.
