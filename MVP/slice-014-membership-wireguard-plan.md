---
title: Slice 014 Membership And Last-Applied WireGuard Plan
status: active
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
---

# Slice 014 Membership And Last-Applied WireGuard Plan

## Problem Frame

The MVP now proves NATS-shaped bus semantics, authority islands, bridges,
docs-backed facts, rebuildable projections, deploy commit-before-drain, ACME
advisory leases, and HTTP/DNS serving through coordinator death. The largest
remaining steady-state gap is membership plus the private data plane:

- `MVP/e2e-proof-plan.md` E2E-5 has no real proof for init, invite, join,
  tombstone, or full-mesh WireGuard reconciliation.
- `MVP/e2e-proof-plan.md` E2E-7 still lacks service-to-service traffic over
  last-applied WireGuard config while the coordinator is down.
- `MVP/architecture.md` says the coordinator should modify local node state,
  but WireGuard/data-plane serving must not share its fate.

The single proof target for this slice:

```text
Ten MVP nodes join through docs-backed membership facts, every live node derives
the same full-mesh WireGuard peer plan excluding itself and tombstoned nodes,
a separate last-applied data-plane role serves real TCP service-to-service
traffic from that plan, and that traffic continues after the coordinator role
is killed.
```

This is not a real privileged kernel WireGuard deployment yet. The slice should
prove the membership and last-applied peer-config semantics with real process
and socket boundaries, while introducing a narrow backend boundary that can
later be implemented by a host/container WireGuard adapter.

## Requirements Trace

- From `VISION.md`: the daemon is disposable; WireGuard and service traffic
  outlive it.
- From `MVP/overall-plan.md`: every node is equal, no hidden quorum or witness
  ack collection, command results report visible nodes at decision time, and a
  future active-member/partition view is deferred explicit evidence rather than
  a commit gate.
- From `MVP/architecture.md`: `ployz init`, `ployz machine invite`, and
  `ployz join` create island-scoped membership facts; clusters up to 32 nodes
  use full-mesh WireGuard.
- From `MVP/e2e-proof-plan.md` E2E-5: prove invite expiration, join, ten-node
  full mesh, tombstone exclusion, and tombstoned reconnect rejection.
- From `MVP/e2e-proof-plan.md` E2E-7: prove coordinator-down
  service-to-service traffic over last-applied WireGuard config.

## Scope Boundaries

In scope:

- MVP-local typed membership identities and facts.
- Island-scoped invite token semantics with expiration.
- Docs-backed join proof using existing `mvp-iroh` fact document sharing/import.
- Overlay IPv6 derivation from cluster/island plus node identity, with no IPAM
  lease table.
- Full-mesh peer planning for up to 32 live, non-tombstoned nodes.
- Last-applied WireGuard config snapshot written atomically and loadable by a
  process role without coordinator liveness.
- A test WireGuard backend/data-plane role that uses real TCP sockets and only
  routes to peers present in the last-applied config.
- Force-remove/tombstone membership semantics and peer exclusion.
- E2E metrics for join duration, docs convergence, peer reconciliation, and
  service-to-service success during coordinator outage.

Out of scope:

- Real kernel, userspace TUN, Docker, or host WireGuard interface mutation in
  the default E2E suite.
- Workload drain, route removal, and runtime stop for graceful machine remove.
  This slice proves the membership/tombstone/WireGuard part; deploy/runtime
  drain remains in the deploy/runtime slices.
- Active-member or active-partition tracking. It may become future
  decision-time evidence, but this slice must not introduce quorum-like commit
  behavior.
- Partial WireGuard graph selection beyond full mesh.
- Custom `/ployz/join/1` iroh RPC. The join proof may use the existing
  iroh-docs ticket/share path; a dedicated join RPC can be added when the bus
  transport slice needs it.

## Crate Scout

Checked before writing new plumbing:

- `defguard_wireguard_rs` 0.9.6 is a maintained high-level API for managing
  WireGuard interfaces through native kernel and userspace implementations:
  <https://docs.rs/defguard_wireguard_rs/latest/defguard_wireguard_rs/>
  It is the best future production adapter candidate because it exposes
  interface and peer configuration as product-level concepts, but it performs
  real host/interface operations and is not appropriate for the always-on E2E
  suite.
- `wireguard-control` 1.7.1 exposes lower-level WireGuard device, peer, allowed
  IP, and device-update types:
  <https://docs.rs/wireguard-control/latest/wireguard_control/>
  It is useful as a reference if the production adapter needs smaller Linux
  control, but the MVP should not bind business semantics to this crate yet.
- `boringtun` 0.7.1 is a userspace WireGuard protocol implementation:
  <https://docs.rs/boringtun/latest/boringtun/>
  It is too low-level for this slice because the proof target is membership and
  last-applied peer planning, not encrypted packet handling.
- Existing code under `crates/ployz-orchestrator/src/mesh` and
  `crates/ployz-runtime-backends/src/mesh` has useful references:
  `MemoryWireGuard`, `MeshNetwork`, `WireGuardDevice`, and sync config
  rendering. Treat those as patterns to study, not code to move.

Decision for this slice:

- Add a tiny MVP-local WireGuard backend trait and a memory/file-backed test
  adapter.
- Do not add `defguard_wireguard_rs`, `wireguard-control`, or `boringtun` as
  dependencies yet.
- Shape the adapter so a later production implementation can use
  `defguard_wireguard_rs` without changing membership or deploy business code.

## Design Decisions

### Membership Facts

Membership truth remains in immutable facts. Extend the projection fact model
with the fields needed to plan the mesh:

```text
facts/node/<node_id>/joined/<epoch>
facts/node/<node_id>/tombstoned/<epoch>
```

The joined fact must carry:

- node id,
- epoch,
- iroh endpoint id/address string used for bootstrap visibility,
- WireGuard public key,
- derived overlay IPv6 address.

The tombstone fact must carry:

- node id,
- epoch,
- reason,
- author/principal context from the fact envelope.

Reducers should keep the latest joined epoch unless a tombstone at an equal or
higher epoch exists. Tombstoned nodes must be excluded from service registry and
WireGuard planning. Conflicting same-epoch candidates remain reducer-visible
and use the existing deterministic conflict/supersession status machinery.

### Overlay IPs

Overlay IPs are derived, not allocated. Use an MVP-local helper that takes the
island/cluster id and node id, hashes them, and produces a stable ULA IPv6
address. The helper should be pure, deterministic, covered by tests, and not
write lease/IPAM state.

### Join And Invite

`machine invite` creates an island-scoped invite token and an invite fact.
`join` validates expiration and invite secret before writing the new node's
joined fact. The command result reports visible nodes at decision time.

The join proof should use `mvp-iroh` docs share/import for the docs-backed fact
path. This keeps iroh in the slice without pretending the PloyzBus transport is
already distributed.

### WireGuard Planning

Planning is pure:

```text
ProjectionState + local node id -> WireGuardPeerPlan
```

For `node_count <= 32`, the plan includes every other live non-tombstoned node.
It excludes:

- the local node,
- tombstoned nodes,
- nodes with malformed/missing mesh identity fields,
- nodes from another island.

Peer config should use typed fields, not display strings:

- peer node id,
- WireGuard public key,
- overlay allowed IP as `/128`,
- optional endpoint hints.

### Last-Applied Data Plane

The coordinator may write or request a new plan, but the applied plan is owned
by a steady-state role. The role:

- loads the last-applied WireGuard snapshot before accepting traffic,
- keeps serving with that snapshot while the coordinator is down,
- rejects routes to peers not present in the applied snapshot,
- can apply an already-authorized tombstone-derived snapshot without reviving
  coordinator mutation authority,
- reports coordinator health separately from data-plane health.

The E2E role can implement traffic with real loopback TCP sockets gated by the
last-applied peer table. That proves OS process fate separation and real socket
traffic without root privileges. The slice report must label this honestly as a
WireGuard config/data-plane harness, not a kernel WireGuard packet proof.

## Implementation Units

### U1: Membership Fact Model And Reducer

Files:

- Create `MVP/mesh/Cargo.toml`
- Create `MVP/mesh/src/lib.rs`
- Create `MVP/mesh/src/domain.rs`
- Modify `MVP/Cargo.toml`
- Modify `MVP/projection/src/facts.rs`
- Modify `MVP/projection/src/model.rs`
- Modify `MVP/projection/src/reducer.rs`
- Modify `MVP/projection/src/source.rs`
- Modify `MVP/projection/src/sqlite.rs`

Approach:

- Introduce MVP-local newtypes for `IrohEndpointId`, `WireGuardPublicKey`, and
  `WireGuardOverlayIp` where mesh code needs typed routing/identity values.
- Extend node projection with mesh identity needed by peer planning.
- Add `NodeTombstoned` fact support and ensure tombstoned nodes are not
  projected as live nodes.
- Keep conflict handling consistent with the existing reducer contract:
  conflict candidates stay visible and deterministic winners/losers are
  reflected through projection status.

Test scenarios:

- Joined node with full mesh identity projects as live.
- Tombstone at higher epoch removes the node from live projection.
- Older tombstone does not remove a newer joined epoch.
- Same-epoch conflicting joined facts surface conflict/superseded status.
- Malformed joined fact key/payload is ignored with visible status.
- SQLite persists and reloads mesh identity and tombstone-reduced live nodes.

Verification:

- `cd MVP && cargo test -p mvp-projection`
- `cd MVP && cargo test -p mvp-mesh`

### U2: Invite And Join Command Semantics

Files:

- Create `MVP/mesh/src/invite.rs`
- Create `MVP/mesh/src/join.rs`
- Create `MVP/mesh/src/remove.rs`
- Modify `MVP/iroh/src/facts.rs` if helper ergonomics are needed for membership
  payload writes
- Create `MVP/e2e/src/membership_wireguard_contract.rs`
- Modify `MVP/e2e/src/main.rs`

Approach:

- Model invite tokens as island-scoped values with bootstrap endpoint info,
  invite id, secret, expiration, and initial grants.
- Validate expiration and secret before writing membership facts.
- Return structured outcomes that include visible nodes at decision time.
- Prove docs-backed join by sharing/importing an iroh-docs fact document and
  waiting for joined facts to converge through `mvp-iroh`.
- Reject tombstoned reconnect attempts unless a fresh reinvite path is
  explicitly present. This slice does not need to implement reinvite; it only
  needs to prove rejection.

Test scenarios:

- First node initializes island and writes its joined fact.
- Valid invite adds a second node through docs-backed join.
- Expired invite fails before any membership mutation.
- Ten nodes join and all see the same live membership set after docs refresh.
- Tombstoned node cannot write a new joined fact through the normal join path.
- Command outcomes include visible nodes at decision time and no witness/quorum
  field.

Verification:

- `cd MVP && cargo test -p mvp-mesh`
- `cd MVP && cargo run -p mvp-e2e -- membership-wireguard-contract`

### U3: Full-Mesh WireGuard Planning And Snapshot Apply

Files:

- Create `MVP/mesh/src/wireguard.rs`
- Create `MVP/mesh/src/snapshot.rs`
- Create `MVP/mesh/src/actor.rs`
- Modify `MVP/primitive-decisions.md`

Approach:

- Add a narrow `WireGuardBackend` trait with an apply/read-last-applied shape,
  not a production host interface API.
- Add a memory/file-backed backend for E2E and unit tests.
- Write last-applied snapshots atomically under the scenario root.
- Make the actor/handle own apply/status, not membership truth.

Test scenarios:

- A ten-node projection produces nine peers for each local node.
- Planned peer sets contain no local node and no tombstoned node.
- Peer planning is deterministic regardless of input fact order.
- Applying a snapshot writes exactly the desired peers and records the revision.
- Invalid next snapshot preserves the previous last-applied config.

Verification:

- `cd MVP && cargo test -p mvp-mesh`
- `cd MVP && cargo clippy -p mvp-mesh --tests -- -D warnings`

### U4: Process-Role Data-Plane E2E

Files:

- Modify `MVP/e2e/src/process_role_harness.rs`
- Modify `MVP/e2e/src/membership_wireguard_contract.rs`
- Modify `MVP/e2e/src/main.rs`

Approach:

- Add a `mesh-data-plane` process role that loads the last-applied mesh snapshot
  and serves a TCP echo endpoint for each node.
- Gate outbound service-to-service requests on the applied peer table. If a
  peer is absent from the last-applied table, the request fails before opening
  the service connection.
- Start coordinator plus data-plane role, apply the full-mesh plan, prove
  service traffic, kill the coordinator, then prove service traffic still
  succeeds.
- Inject an already-authorized tombstone-derived snapshot and prove the
  data-plane role can reload peer removal while coordinator mutation authority
  remains unavailable.

Test scenarios:

- Service-to-service TCP request succeeds before coordinator death.
- Coordinator death makes local membership mutation unavailable.
- Service-to-service TCP request still succeeds after coordinator death.
- Tombstoned peer is removed from the applied table and subsequent traffic to
  that peer fails with a structured peer-not-applied error.
- Restarting the data-plane role while the coordinator is down loads the
  last-applied snapshot and serves traffic.

Verification:

- `cd MVP && cargo run -p mvp-e2e -- membership-wireguard-contract`
- `cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all`

### U5: Slice Documentation And Semantic Leverage Report

Files:

- Add `MVP/slice-014-membership-wireguard.md`
- Modify `MVP/e2e-proof-plan.md`
- Modify `MVP/primitive-decisions.md`

Approach:

- Record what the slice proves and what it deliberately does not prove.
- Record the crate scout decision: future production host adapter likely uses
  `defguard_wireguard_rs`, but the always-on E2E path uses a local backend.
- Add a semantic-leverage note comparing the new mesh/join planning surface to
  the old mesh reference files listed in `MVP/overall-plan.md`.
- Keep the future active-member/partition-view idea documented as deferred
  explicit evidence, not a hidden commit or lease boundary.

Verification:

- Slice report names all tests run.
- Primitive decisions include the new membership/WireGuard entries.
- E2E proof status updates E2E-5 and E2E-7 without overstating real kernel
  WireGuard coverage.

## Review Risks

- The data-plane harness could be overclaimed as real WireGuard. The docs and
  report must be precise: this proves peer-plan application and process fate
  separation, not encrypted kernel tunnel behavior.
- A background applier must not silently rewrite durable truth. It may apply
  already-committed facts/snapshots; it must not invent membership state.
- Tombstone logic can accidentally become a freshness/liveness inference. Only
  explicit tombstone facts remove nodes from live membership.
- Adding mesh identity to existing node facts may make old tests noisy. Keep
  fixture helpers small and typed instead of sprinkling raw strings.
- The future active-member idea must stay out of the commit path. Visible nodes
  are command evidence only.

## Execution Cadence

- Commit this plan separately.
- Implement U1-U2 in one or two small commits, depending on reducer churn.
- Implement U3 separately.
- Implement U4 E2E separately.
- Run `ce-simplify-code` after the first passing `membership-wireguard-contract`
  and commit simplification separately.
- Run `ce-code-review` with subagents before the final review-fix commit.
- Push after the plan commit and after the completed slice.

## Acceptance Gate

The slice is complete when:

- `cd MVP && cargo fmt --all -- --check` passes.
- `cd MVP && cargo test -p mvp-projection` passes.
- `cd MVP && cargo test -p mvp-mesh` passes.
- `cd MVP && cargo run -p mvp-e2e -- membership-wireguard-contract` passes.
- `cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all` passes.
- The branch includes separate plan, implementation, simplification, and
  review-fix commits where applicable.
