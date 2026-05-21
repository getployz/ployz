---
title: Slice 014 Membership And Last-Applied WireGuard
status: completed
plan: MVP/slice-014-membership-wireguard-plan.md
created: 2026-05-18
---

# Slice 014 Membership And Last-Applied WireGuard

## Result

This slice adds the first machine membership and private data-plane canary under
`MVP/`.

- `mvp-mesh` defines typed invite, join, tombstone, mesh identity, overlay IP,
  full-mesh peer planning, last-applied snapshot, and Kameo WireGuard actor
  primitives.
- `NodeJoined` facts now carry iroh endpoint id and WireGuard public key for
  mesh planning.
- `NodeTombstoned` facts project as durable exclusion. Normal future join or
  service facts do not resurrect that node id; a later slice must define an
  explicit reinvite/clear primitive if we need one.
- Join and tombstone writes use different fact grants, so a join writer cannot
  remove machines.
- `mvp-e2e role mesh-data-plane` is a separate OS process role that loads a
  last-applied mesh snapshot and serves loopback TCP traffic gated by that
  peer table.
- The data-plane role resolves outbound target sockets from the applied
  snapshot, not from caller-supplied addresses.
- `membership-wireguard-contract` proves ten docs-backed joins, 90 full-mesh
  peer relationships, tombstone projection, tombstoned rejoin rejection,
  coordinator-death mutation failure, and service-to-service traffic before and
  after coordinator death.

This is deliberately not a kernel WireGuard packet proof. It proves the
membership, applied-config, and process fate semantics that a production
WireGuard adapter must preserve.

## Crate Decisions

Checked before implementation:

- `defguard_wireguard_rs` remains the best production adapter candidate for
  managing host/userspace WireGuard interfaces, but it is too privileged for
  the always-on MVP E2E suite.
- `wireguard-control` is a useful lower-level Linux reference, but the business
  model should not depend on its device-update shape yet.
- `boringtun` is too low-level for this slice because encrypted packet handling
  is not the current proof target.
- Existing mesh code under `crates/ployz-orchestrator/src/mesh` and
  `crates/ployz-runtime-backends/src/mesh` was used as reference only.

The slice adds only a narrow `WireGuardBackend` trait, a memory backend, and
file-backed last-applied snapshots. A production adapter can be added behind
that boundary later.

## Proof

Checks run:

```text
cd MVP && cargo fmt --all
cd MVP && cargo test -p mvp-projection -p mvp-mesh -p mvp-e2e
cd MVP && cargo fmt --all -- --check
cd MVP && cargo clippy -p mvp-projection -p mvp-mesh -p mvp-e2e --tests -- -D warnings
cd MVP && cargo run -p mvp-e2e -- membership-wireguard-contract
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

Observed `membership-wireguard-contract` metrics:

```text
joined_nodes: 10
total_planned_peers: 90
post_tombstone_nodes: 9
post_tombstone_node0_peers: 8
expired_invite_failed: true
tombstoned_rejoin_failed: true
join_writer_tombstone_denied: true
coordinator_mutation_unavailable: true
traffic_before_coordinator_death: true
traffic_after_coordinator_death: true
traffic_after_data_plane_restart: true
tombstoned_peer_rejected: true
join_duration_ms: 11
docs_convergence_ms: 59
peer_plan_duration_ms: 0
data_plane_outage_success_count: 3
elapsed_ms: 536
```

## Review Fixes

Review caught several issues that were fixed before completing the slice:

- Tombstones now dominate future normal joins/services until an explicit
  reinvite/clear primitive exists.
- Tombstoned nodes remove all projected services regardless of service epoch.
- Reduced projection state now carries tombstoned node ids/epochs, so join
  admission can consume the same membership view as future business logic.
- Malformed remote mesh identity is skipped during peer planning, while invalid
  local identity still fails the plan.
- The E2E mesh send command no longer accepts an arbitrary target socket.
- Tombstoned rejoin rejection is derived from replicated tombstone facts, not a
  manually injected test set.
- Coordinator-down mutation proof now attempts a real mutation.
- Last-applied snapshot writes use random same-directory temp files, sync the
  file, reject symlink targets, and persist atomically.
- The WireGuard actor bounds backend apply time and remains responsive after an
  apply timeout.
- `mvp-e2e -- all` timeout errors always include mesh process cleanup status.

## Semantic-Leverage Check

Old mesh reference baseline:

```text
crates/ployz-orchestrator/src/mesh + crates/ployz-runtime-backends/src/mesh: 6448 LOC
crates/ployzd mesh state/handlers sample: 2940 LOC
```

New MVP mesh canary:

```text
MVP/mesh/src/*.rs: 1208 LOC
MVP/e2e/src/membership_wireguard_contract.rs: 573 LOC
```

The new code does not have production WireGuard mutation yet, so this is not a
feature-complete LOC replacement. The leverage signal is the shape: membership
business rules are typed invite/join/tombstone commands plus pure peer planning,
while process fate and traffic proof live in the harness instead of leaking into
membership reducers.

## Covered And Deferred

Covered:

- Island-scoped invite expiration and secret validation.
- Docs-backed joined fact replication through `mvp-iroh`.
- Ten-node full-mesh peer planning for live projected membership.
- Derived overlay IPs with no IPAM lease table.
- Force-remove tombstone projection and peer exclusion.
- Tombstoned rejoin rejection from replicated tombstone facts.
- Split join/tombstone fact grants.
- Last-applied mesh data-plane process role.
- Service-to-service traffic while the coordinator is dead.
- Data-plane role restart from snapshot while the coordinator remains dead.
- Secure-ish MVP snapshot replacement and bounded backend apply failure.

Deferred:

- Real kernel/userspace WireGuard interface mutation.
- Graceful machine remove with workload drain, route removal, and runtime stop.
- Production `/ployz/join/1` iroh/PloyzBus RPC.
- Explicit reinvite/clear primitive for tombstoned node ids.
- Partial WireGuard graph selection beyond full mesh.
- Future active-member/partition-view evidence. It may improve command
  preconditions later, but it is not part of the commit boundary.
