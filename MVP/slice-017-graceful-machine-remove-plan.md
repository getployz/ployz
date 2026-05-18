---
title: Slice 017 Graceful Machine Remove Plan
status: active
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-014-membership-wireguard-plan.md
  - MVP/slice-016-identity-routing-boundaries-plan.md
---

# Slice 017 Graceful Machine Remove Plan

## Problem Frame

The MVP can now add machines, project tombstones, remove WireGuard peers from
last-applied snapshots, and keep service-to-service traffic alive while the
local coordinator is down. That proves force-remove membership behavior, but it
does not prove the product primitive the operator actually needs:

```text
ployz machine remove <node_id>
```

Graceful remove is the next best product proof because it forces several MVP
primitives to compose without falling back to the old monolithic deploy shape:

- explicit operator intent facts,
- visible nodes at decision time,
- targeted request/reply to one node,
- route cutover as a durable local fact,
- projection catch-up before destructive cleanup,
- tombstone after cleanup,
- WireGuard peer removal after tombstone,
- steady-state data-plane continuity for the remaining nodes.

The single proof target for this slice:

```text
A graceful machine-remove command drains one live node out of serving traffic,
commits a local durable serving-state fact, waits for projection evidence,
stops the target workload, tombstones the node, removes it from WireGuard peer
plans, and leaves remaining service traffic working.
```

This slice is not a port of any old daemon command. It is a semantic-leverage
proof that machine remove can be expressed as a small product command over the
new bus, fact, projection, routing, and mesh primitives.

## Requirements Trace

- `VISION.md`: `machine remove` is a north-star primitive. It must have visible
  preconditions, bounded effects, a clear result, and a verification path.
- `VISION.md`: the data plane outlives the control plane. Removing one node
  must not interrupt already-running traffic between remaining nodes.
- `MVP/overall-plan.md`: the operator's connected node is the consistency
  boundary. The command writes durably to local docs/facts and returns; it does
  not collect witness acknowledgements.
- `MVP/architecture.md`: graceful remove writes removal intent, stops new work,
  removes active backends from routes, waits drain policy, stops workloads,
  tombstones the node, and removes WireGuard peers.
- `MVP/e2e-proof-plan.md`: E2E-5 still lacks graceful remove with workload
  drain, route removal, runtime stop, and peer exclusion. This slice covers the
  fixture-backed command and wire-proof subset; real runtime and production
  WireGuard backend mutation stay deferred.
- `MVP/primitive-decisions.md`: force-remove tombstones are proven, but
  workload drain and route removal are explicitly deferred.
- `MVP/slice-016-identity-routing-boundaries-plan.md`: node identity and
  visible-node evidence now have one shared representation, so this slice can
  add a node-facing command without choosing between parallel wrappers.

## Scope

In scope:

- Add a graceful machine-remove product command under `MVP/`.
- Introduce a `NodeRemovalStarted` fact and projection state for "no new work"
  evidence without deleting existing live membership or service state early.
- Extract the route/serving commit primitive out of `mvp-deploy`.
- Add participant RPC types for target-node remove preparation and final stop.
- Commit a serving-state fact that removes target-node backends from active
  routes while keeping old backends alive long enough for projection lag.
- Require projection catch-up evidence before final stop/tombstone.
- Write `NodeTombstoned` only after successful target final stop in the graceful
  path.
- Preserve force-remove tombstone behavior from Slice 014.
- Add an E2E scenario proving route removal, tombstone projection, WireGuard
  peer exclusion, and remaining data-plane traffic.
- Keep all changes self-contained under `MVP/`.

Out of scope:

- Kernel/userspace WireGuard interface mutation through a production backend.
- Real container runtime integration. Participant handlers remain typed E2E
  fixtures that prove ordering, drain acknowledgement, and error surfaces.
- Self-removal of the operator's connected node.
- Reinvite/clear for tombstoned node ids.
- Active-member or partition-view quorum checks.
- Automatic rollback if route commit succeeds and later cleanup fails.
- `mvp-commands` / `PhasedCommand`; this slice should stay explicit unless the
  implementation reveals a third repeated phase/resume command shape.
- Migration into existing `crates/` code.

## Crate Scout

Checked before planning:

- `tokio-util` 0.7.18 exposes `CancellationToken`, a future that resolves when
  the token is cancelled:
  <https://docs.rs/tokio-util/latest/tokio_util/sync/index.html>. This is a good
  future fit for role shutdown and long-lived appliers, but graceful remove is
  a foreground command with bounded bus deadlines. Do not add it just for this
  slice unless implementation introduces cancellable background workers.
- `petgraph` 0.8.3 provides graph types and algorithms:
  <https://docs.rs/petgraph/>. It remains unnecessary for the MVP full-mesh
  peer plan and one-target removal. Revisit only when partial WireGuard graph
  selection becomes a real product proof.
- `governor` 0.10.4 is an efficient rate-limiting crate:
  <https://docs.rs/governor>. This could shape future rollout/drain rate
  limits, but this slice needs deterministic bounded participant RPC, not
  distributed rate limiting.

Decision for this slice:

- Do not add a new dependency by default.
- Prefer extracting existing serving-commit code into a small MVP-local
  primitive over pulling in a workflow/rate/graph crate.
- Keep participant orchestration in ordinary async Rust plus existing
  `BusActorHandle` request/reply semantics.

## Design Decisions

### Product Command Boundary

Graceful machine remove is not a mesh-only operation. It touches membership,
serving routes, participant RPC, and WireGuard peer planning. Do not bury the
product command in a WireGuard adapter.

Preferred shape:

```text
MVP/machine
  domain types for MachineRemoveRequest/Result/Error
  coordinator for graceful remove
  participant wire payloads for prepare/stop
```

`mvp-machine` may depend on bus, identity, projection, mesh, and a shared
serving-route primitive. It should not depend on `mvp-deploy` just to write a
serving commit.

If implementation shows that a whole crate is too much for the first proof,
`MVP/mesh/src/remove.rs` is acceptable only if it keeps the orchestration
boundary explicit and does not turn mesh into deploy/runtime state.

### Shared Serving Commit Primitive

Slice 010 put `ServingCommitPlan`, `ProjectionCatchUp`, and
`write_serving_commit` in `mvp-deploy` because deploy was the first user. Machine
remove becomes the second user. At that point "route cutover as durable fact"
is a primitive, not deploy-owned behavior.

Extract the smallest useful surface into `MVP/routing`:

- serving/gateway/DNS commit ids,
- serving commit plan,
- fact writer for `ProjectionFactPayload::ServingCommit`,
- projection catch-up proof.

This belongs in `routing`, not `serving`, because it is durable route/DNS/gateway
truth and projection proof. `mvp-serving` owns last-good HTTP/DNS in-memory
serving and wire roles. Keeping the fact-level commit primitive below serving
prevents serving from becoming a catch-all control-plane crate.

Deploy should import that primitive after extraction. Machine remove should use
the same primitive to avoid duplicate route-commit logic.

### Removal Intent Is Not Tombstone

`NodeRemovalStarted` means:

- the operator has started removing this node,
- new placement should exclude it,
- existing services and routes remain live until route cutover says otherwise.

It must not delete membership, remove services, or remove WireGuard peers by
itself. Tombstone remains the fact that removes the node from live membership
and service projections.

Projection should expose removing nodes separately from tombstoned nodes so a
future deploy/scheduler command can fail fast when asked to place new work on a
node already in removal.

### Fact Write Boundary

Machine remove needs docs-backed membership facts and a bus-backed serving
commit in the same ordered command. Do not hide that behind a fake store facade.

Use an explicit command fact writer boundary:

```text
MachineFactWriter
  write_removal_started(fact)
  write_tombstone(fact)
```

The E2E implementation can back this with `IrohFactDoc::write_fact_payload`,
using a principal that has grants for:

```text
/facts/node/*/removal_started/>
/facts/node/*/tombstoned/>
```

The shared routing primitive continues to write `ServingCommit` through the
existing bus fact-write path. The machine-remove coordinator owns ordering
across those two sinks, and tests should assert the recorded order.

### Graceful Remove Ordering

The command should make the destructive boundary visible:

1. Read projection/facts and fail before mutation if the target is missing,
   tombstoned, or already being removed by an incompatible fact.
2. Record `visible_nodes` in the command result.
3. Probe target prepare/drain responder availability through the bus. If there
   is no responder, fail before mutation and suggest force remove to the caller.
4. Write `NodeRemovalStarted`.
5. Request target prepare/drain:
   `node.<id>.rpc.prepare_remove`.
6. Require `PrepareRemoveReply` to declare the participant drain state:
   `NoNewWorkAndDrained` for this MVP.
7. Write a serving commit that removes target backends from active route
   backends and lists them as old backends to drain.
8. Wait for `ProjectionCatchUp` from local projection/snapshot output.
9. Request target final stop:
   `node.<id>.rpc.stop_removed_workloads`.
10. Write `NodeTombstoned`.
11. Reproject membership/WireGuard and verify the target peer is gone.

If steps 3, 5, 6, or 7 fail, do not tombstone. The result should be a structured
foreground failure. If steps 8 or 9 fail after the serving commit exists, return
a recoverable cleanup-pending result that includes the serving commit id,
visible nodes, and target node id. Do not pretend the remove succeeded.

The MVP drain contract is intentionally narrow: `PrepareRemoveReply` must mean
the target has stopped accepting new work and has no in-flight work for the
backends the command is removing. There is no separate grace timer in this
slice. When real runtime integration lands, the participant implementation may
spend that time draining before it returns `NoNewWorkAndDrained`.

### Visible Nodes, Not Quorum

The command reports visible nodes at decision time. It does not require witness
acks, `min_replicas`, or a live-majority view.

If the target has no responder for `prepare_remove`, that is a direct
pre-mutation precondition failure for graceful remove. Operators can still use
force remove, which is already the tombstone-only path.

### PhasedCommand

This command will likely look phase-shaped. Keep it explicit in this slice.

The trigger for `mvp-commands` is still three or more commands with phase enums,
resume-from-phase logic, and non-trivial compensation. Today deploy has the
clear shape; graceful machine remove may become the second. Do not introduce
`PhasedCommand` until the repeated pattern is proven rather than predicted.

## Implementation Units

### Unit 1: Extract Serving Commit Primitive

Files:

- `MVP/Cargo.toml`
- `MVP/routing/Cargo.toml`
- `MVP/routing/src/lib.rs`
- `MVP/deploy/Cargo.toml`
- `MVP/deploy/src/domain.rs`
- `MVP/deploy/src/serving_commit.rs`
- `MVP/deploy/src/coordinator.rs`
- `MVP/deploy/src/tests.rs`

Work:

- Move shared serving-commit ids, `ServingCommitPlan`, `ProjectionCatchUp`, and
  `write_serving_commit` out of deploy-specific ownership.
- Keep `mvp-routing` limited to those exports plus supporting error types; it is
  not a serving actor, HTTP/DNS role, deploy coordinator, or route planner.
- Keep deploy behavior and wire payloads unchanged.
- Keep projection facts unchanged.
- Add unit tests around catch-up proof in the new primitive if the moved tests
  would otherwise stay deploy-specific.

Tests:

- `cargo test -p mvp-routing --lib`
- `cargo test -p mvp-deploy --lib`
- `cargo run -p mvp-e2e -- deploy-commit-drain-contract`

### Unit 2: Removal Facts And Projection State

Files:

- `MVP/projection/src/facts.rs`
- `MVP/projection/src/model.rs`
- `MVP/projection/src/reducer.rs`
- `MVP/projection/src/sqlite.rs`
- `MVP/projection/src/snapshot.rs`
- `MVP/projection/src/actor.rs`
- `MVP/projection/src/tests` or existing reducer test module
- `MVP/mesh/src/invite.rs`

Work:

- Add `NodeRemovalStartedFact`.
- Add fact-key classification for `/facts/node/<node_id>/removal_started/<epoch>`.
- Project removal-started state separately from tombstones.
- Ensure tombstone still dominates joined/service state.
- Ensure removal-started does not remove active routes or services on its own.
- Add a helper for building removal-started fact keys beside joined/tombstone
  helpers, or move membership fact-key helpers into a clearer module if needed.

Tests:

- Removal-started is projected as no-new-work evidence.
- Removal-started does not delete existing services or routes.
- Tombstone still removes node services and live membership.
- Higher-epoch or same-epoch conflicts are deterministic and surfaced as
  projection status, following existing reducer rules.

### Unit 3: Graceful Machine Remove Command

Files:

- `MVP/Cargo.toml`
- `MVP/machine/Cargo.toml`
- `MVP/machine/src/lib.rs`
- `MVP/machine/src/remove.rs`
- `MVP/machine/src/wire.rs`
- `MVP/machine/src/error.rs`
- `MVP/mesh/src/lib.rs`

Work:

- Add product-level remove request/result/error types.
- Add participant payloads:
  - `PrepareRemoveRequest`
  - `PrepareRemoveReply`
  - `PrepareRemoveOutcome::NoNewWorkAndDrained`
  - `StopRemovedWorkloadsRequest`
  - `StopRemovedWorkloadsReply`
- Add an explicit `MachineFactWriter` trait or equivalent narrow boundary for
  removal-started and tombstone docs-backed facts.
- Add a coordinator that:
  - reads current projection/facts supplied by the caller,
  - fails before mutation for missing/tombstoned/already-removing target,
  - probes target prepare responder availability before mutation,
  - writes removal-started through `MachineFactWriter`,
  - requests target prepare and requires `NoNewWorkAndDrained`,
  - writes serving commit through the shared routing primitive,
  - requires projection catch-up before final stop,
  - writes tombstone through `MachineFactWriter` after successful stop,
  - returns visible nodes and structured cleanup status.
- Keep errors structured. Callers must branch on variants, not display strings.

Tests:

- Missing target fails before any fact write.
- Tombstoned target fails before any fact write.
- No responder on `prepare_remove` fails before any fact write, serving commit,
  or tombstone.
- Prepare reply that is not `NoNewWorkAndDrained` fails before serving commit or
  tombstone.
- Serving commit failure leaves only removal-started intent.
- Projection catch-up mismatch returns cleanup pending and does not tombstone.
- `stop_removed_workloads` timeout/failure returns cleanup pending with target
  node id, visible nodes, and serving commit id, and does not tombstone.
- Successful remove writes removal-started, serving commit, stop, tombstone in
  that order.

### Unit 4: E2E Graceful Remove Proof

Files:

- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/machine_remove_contract.rs`
- `MVP/e2e/src/process_role_harness.rs`
- `MVP/e2e-proof-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/slice-017-graceful-machine-remove.md` after implementation

Work:

- Add `machine-remove-contract`.
- Build a small docs-backed membership/projection setup with four logical nodes:
  - one target node,
  - one remaining source backend node,
  - one remaining destination backend node,
  - one operator/observer node.
- Publish an initial serving commit with target and remaining backend active.
- Register bus participant handlers for target prepare/stop.
- Run graceful remove.
- Rebuild projection and snapshots.
- Replan/reload WireGuard snapshots.
- Verify remaining source-to-destination service traffic still works and
  removed-target traffic is rejected from the applied peer table.
- Emit structured metrics.

Required assertions:

- `visible_nodes_at_decision` includes the nodes the command saw.
- `NodeRemovalStarted` appears before route cutover.
- The route commit excludes the target from active backends.
- Old target backend remains in the drain/old-backend set until cleanup.
- Prepare reply proves `NoNewWorkAndDrained`.
- Stop request happens only after projection catch-up evidence.
- `NodeTombstoned` appears only after stop success.
- Force-remove behavior from `membership-wireguard-contract` remains green.
- Remaining peer traffic succeeds after target peer removal.

Metrics:

- remove duration,
- prepare RPC latency,
- route commit to projection latency,
- projection rebuild latency,
- tombstone convergence latency,
- WireGuard peer-plan duration,
- remaining traffic success count,
- cleanup-pending count.

Tests:

- `cargo run -p mvp-e2e -- machine-remove-contract`
- `cargo run -p mvp-e2e -- membership-wireguard-contract`
- `cargo run -p mvp-e2e -- deploy-commit-drain-contract`
- `MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all`

## Proof Criteria

The slice is complete when:

- Successful graceful remove has a product-level command result with visible
  nodes, route/serving commit id, cleanup status, and tombstone fact key.
- Cleanup-pending graceful remove results include visible nodes,
  route/serving commit id, target node id, and no tombstone fact key.
- Force remove remains tombstone-only and does not wait for participant RPC.
- A no-responder target fails graceful remove before removal-started, route
  commit, or tombstone.
- The target participant acknowledges `NoNewWorkAndDrained` before route commit.
- A route commit is projected before target stop.
- Tombstone is written only after successful stop in the graceful path.
- Projection rebuild from facts reproduces post-remove membership and serving
  state.
- WireGuard peer plans exclude the removed node after tombstone.
- Remaining service-to-service traffic survives the remove.
- Full MVP E2E remains within the existing time budget.

## Semantic-Leverage Check

Before implementation, record the current shape:

```text
rg -n "force_remove|TombstoneCommand|DeployCoordinator|ServingCommitPlan|ProjectionCatchUp" MVP
```

After implementation, inspect:

- How many files change to add the graceful-remove business rule.
- Whether machine remove depends on a shared routing primitive instead of
  deploy internals.
- Whether the command body reads as product order:
  preflight -> removal intent -> prepare -> route commit -> projection proof ->
  stop -> tombstone.
- Whether E2E tests assert business invariants directly rather than scripting
  storage or transport details.

The target is not fewer total lines at all costs. The target is that the
machine-remove business invariant is visible without reading bus, iroh-docs,
SQLite, or process-role internals.

## Review Risks

- Route commit extraction could accidentally become a broad "serving framework".
  Keep it to the shared fact/write/projection-proof surface that deploy and
  machine remove both need.
- `NodeRemovalStarted` can easily become hidden desired state. It must be
  command evidence and scheduler/preflight input, not a background controller.
- Tombstoning before route cutover/projection evidence would break the graceful
  contract by removing identity before traffic is safely drained.
- Calling target stop before projection catch-up would repeat the deploy
  drain-before-commit failure class.
- A participant no-responder must be foreground failure for graceful remove,
  not a reason to silently force-remove.
- The E2E harness may tempt implementation toward process plumbing. Keep the
  product invariant in `mvp-machine`; process roles are proof fixtures.

## Suggested Commit Shape

For this slice, keep commits smaller than Slice 016:

1. Plan document.
2. Shared routing/serving commit extraction with deploy proof still green.
3. Removal facts/projection state.
4. Machine remove command plus focused tests.
5. E2E proof.
6. Simplification pass.
7. Review-fix follow-up if review catches invariant bugs.
