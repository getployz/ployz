---
title: MVP Data-Plane Parity Slice 2 WireGuard Backend
status: active
created: 2026-05-20
type: feature
parent_plan: docs/plans/2026-05-20-002-feat-mvp-data-plane-parity.md
---

# MVP Data-Plane Parity Slice 2 WireGuard Backend

## Problem Frame

Slice 1 made the runtime boundary real enough to run Docker containers. Slice 2
must do the same for the private host overlay: keep the existing pure
full-mesh plan, then add a real Linux WireGuard backend that can apply and
adopt local interface state without making membership, projection, or deploy
logic know about Linux tooling.

This slice is not container overlay networking, service DNS, or eBPF routing.
Those depend on a real host WG interface, but they need their own slice because
Docker bridge attachment and eBPF route programming are separate concepts.

## Requirements From Parent Plan

- Covers R2: WireGuard application uses real Linux interfaces/routes and is
  restart adoptable; daemon restart must not tear down the overlay.
- Prepares R4/R5 by making overlay addresses routable at the host layer before
  containers are attached to the overlay.
- Preserves R1 equal-node behavior: every node applies the same kind of local
  WireGuard role from its own projected membership state.
- Keeps backend-specific WireGuard and Linux route code below `mvp-mesh`.

## Current Code Shape

- `MVP/mesh/src/wireguard.rs` already owns pure planning and the
  `WireGuardBackend` trait:
  `apply(WireGuardAppliedSnapshot)` and `last_applied()`.
- `MVP/mesh/src/snapshot.rs` already owns atomic last-applied snapshot files.
- `MVP/mesh/src/linux.rs` is a temporary host-network reachability snapshot.
  It should not grow into overlay mutation.
- `MVP/node/src/membership.rs` starts the current in-process node-agent and
  membership daemon, but does not yet compose a real WireGuard backend.
- `MVP/e2e/src/membership_wireguard_contract.rs` proves membership and
  last-applied semantics with a non-privileged harness, not kernel WG packets.

## Existing Code To Port From

Treat these as preferred source material:

- `crates/ployz-runtime-backends/src/mesh/wireguard/host.rs`
  - `defguard_wireguard_rs` kernel/userspace API wrapper.
  - Interface create/configure/remove.
  - Peer diffing through read/configure/remove peer operations.
  - Linux route add/delete behavior for `fd00::/8`.
  - Per-peer IPv4 route synchronization shape, deferred here until container
    overlay networking needs it.
- `crates/ployz-runtime-backends/src/mesh/wireguard/config.rs`
  - WireGuard key base64 encode/decode helpers.
  - File-backed private-key and sync-config rendering patterns.
- `crates/ployz-bpfctl/src/linux.rs` and `ebpf/`
  - eBPF TC attach/map/route mechanics.
  - Defer this to Slice 3 unless host WG requires a minimal observation hook.
  - Do not mix eBPF bridge routing into this WireGuard host-backend slice.

Port mechanics into `mvp-mesh`; do not import old orchestration/runtime types
upward. The MVP owner concepts remain `WireGuardPeerPlan`,
`WireGuardAppliedSnapshot`, and `WireGuardBackend`.

## Design Decisions

### Real Backend Lives In `mvp-mesh`

The production adapter belongs behind `mvp_mesh::WireGuardBackend`, likely in
small modules under `MVP/mesh/src/wireguard/` or `MVP/mesh/src/linux/`. The
public command/domain crates should only see typed snapshots and backend
reports.

### Use `defguard_wireguard_rs` For Host Interface Mutation

The previous code already proved this is the right high-level API for Linux
kernel/userspace WireGuard operations. Prefer porting that shape over shelling
out to `wg` for peer management. Shelling out to `ip route` is acceptable for
route replacement where the previous implementation already did that.

### Key Material Is Node State, Not Runtime Scratch

WireGuard private key material must be generated or loaded during node init and
join, stored under node-owned paths, and never regenerated on daemon restart.
The public key in membership facts must match that private key. Restart
adoption means loading the same key and reconciling the existing interface.

### Apply Is Bounded And Adoptable

`apply(snapshot)` should:

- create or adopt the interface,
- configure the local overlay address and listen port,
- reconcile peers to the snapshot,
- install required host routes,
- persist the last-applied snapshot only after successful apply.

`last_applied()` remains snapshot-backed. A later status/readiness command can
compare snapshot vs live interface, but this slice should not invent a
background reconciler.

### eBPF Is Explicitly Deferred To Container Networking

The existing eBPF code is valuable, but this slice should not attach TC
classifiers or program container subnet maps. That belongs to Slice 3, where
Docker bridge/subnet ownership and service DNS are being designed together.

## Implementation Units

### Unit 1: WireGuard Key Material

Files:

- `MVP/mesh/src/domain.rs`
- `MVP/mesh/src/wireguard/key.rs` or equivalent
- `MVP/node/src/config.rs`
- `MVP/node/src/state.rs`
- `MVP/node/src/membership.rs`

Work:

- Add generated private-key storage under `NodePaths`.
- Derive/load the public key used in joined facts from the private key.
- Keep existing test fixtures deterministic where needed.
- Validate that daemon restart reloads the same private/public key pair.

Tests:

- Key encode/decode round trips.
- Init/join writes stable key material.
- Reloading node state does not rotate WireGuard identity.

### Unit 2: Linux WireGuard Backend Modules

Files:

- `MVP/mesh/Cargo.toml`
- `MVP/mesh/src/wireguard.rs` or `MVP/mesh/src/wireguard/mod.rs`
- `MVP/mesh/src/wireguard/linux.rs`
- `MVP/mesh/src/error.rs`

Work:

- Add a Linux-gated `LinuxWireGuardBackend`.
- Port the useful `defguard_wireguard_rs` host adapter mechanics:
  interface create/adopt/configure, peer read/configure/remove, route replace,
  and idempotent missing-interface handling.
- Keep route operations bounded and return structured `MeshError::Backend`
  failures with operation context.
- Keep the memory backend as the fast fixture.

Tests:

- Unit tests for peer conversion and route-command planning without needing
  privileges.
- Linux/WG-gated integration test that applies a single-node snapshot and
  reads `last_applied`.

### Unit 3: Snapshot-Backed Apply Semantics

Files:

- `MVP/mesh/src/snapshot.rs`
- `MVP/mesh/src/wireguard.rs`
- `MVP/mesh/src/actor.rs`

Work:

- Ensure backend apply writes last-applied only after live apply succeeds.
- Keep actor apply timeout behavior from the prior MVP membership slice.
- Add status data that distinguishes configured snapshot from live backend
  failure without mutating membership facts.

Tests:

- Failed backend apply does not advance last-applied.
- Timed-out apply leaves previous snapshot visible.
- Successful apply after failure advances snapshot.

### Unit 4: Node Composition And CLI Surface

Files:

- `MVP/node/src/membership.rs`
- `MVP/node/src/main.rs`
- `MVP/node/src/config.rs`
- `MVP/e2e/src/membership_wireguard_contract.rs`

Work:

- Add a feature-gated Linux WireGuard runtime selection for daemon/product
  data-plane mode.
- Keep non-privileged tests on the memory/file backend.
- Make the daemon report whether it registered memory or Linux WireGuard
  backend; do not hide backend unavailability behind a fallback.
- Do not add background reconciliation. Apply on startup/deploy-command paths
  and surface errors to the caller.

Tests:

- Existing membership WireGuard contract remains green.
- Feature-gated Linux smoke applies/adopts a real interface when running with
  the required privileges.

## Acceptance Checklist

- [x] `mvp-mesh` has a real Linux WireGuard backend behind `WireGuardBackend`.
- [x] Existing membership/deploy domain crates do not import `defguard_wireguard_rs`
  or Linux command details.
- [x] WireGuard key material is stable across node reload/daemon restart.
- [ ] Applying a snapshot is idempotent and bounded.
- [x] Last-applied snapshot does not advance on failed live apply.
- [x] Existing non-privileged MVP tests remain green.
- [x] A Linux-gated smoke proves real interface apply/adoption, or records a clear
  privilege/Docker-host blocker if the current environment cannot run it.

## Verification Commands

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-mesh`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node membership`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-e2e membership_wireguard_contract`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-mesh --features linux-wireguard`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node --features linux-wireguard membership`
- `MVP_LINUX_WG_SMOKE=1 cargo test --manifest-path MVP/Cargo.toml -p mvp-mesh --features linux-wireguard linux_wireguard_backend_applies -- --nocapture`

## Completion Evidence

- `d2093145`: node init/join now persist real X25519/WireGuard private key
  material under `NodePaths::wireguard_private_key`, derive the membership
  public key from it, and reject mismatched key/state on load.
- `1cedd947`: `mvp-mesh` gained a feature-gated Linux backend behind
  `WireGuardBackend`, porting the prior `defguard_wireguard_rs` host-interface
  mechanics for interface configuration, peer reconciliation, route
  replacement, and snapshot persistence.
- `64e11cc1`: daemon membership runtime now projects membership facts into a
  full-mesh `WireGuardAppliedSnapshot` and applies it through the existing
  WireGuard actor. Default daemon mode remains memory-backed; Linux mode is
  explicit via `--linux-wireguard-ifname`.
- The current environment is not root (`id -u` returned `1001`), so the real
  interface smoke is present and feature-gated but was not run with
  `MVP_LINUX_WG_SMOKE=1` here. The non-privileged feature-gated smoke command
  verified the test path skips cleanly when not enabled.

## Explicit Deferrals

- Docker bridge attachment and container subnet routing move to Slice 3.
- eBPF TC attach/map programming moves to Slice 3 unless a minimal host-only
  observation hook is required.
- Service DNS moves to Slice 3.
- Pingora, HTTPS, and ACME remain later slices.
