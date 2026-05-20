---
title: MVP Data-Plane Parity Slice 6 Real Install Bootstrap
status: active
created: 2026-05-20
type: feature
parent_plan: docs/plans/2026-05-20-002-feat-mvp-data-plane-parity.md
---

# MVP Data-Plane Parity Slice 6 Real Install Bootstrap

## Problem Frame

The MVP has real runtime, WireGuard, service DNS, Pingora TLS, and Pebble ACME
pieces, but the current product E2E path still leans on in-process helpers and
developer-build assumptions. The final parity push needs an install/bootstrap
surface that can be used by a smoke runner and by a Linux operator without
hand-editing node state.

This slice proves the transition from "built in the repo" to "installed and
bootstrapped as a product binary." It does not finish the full three-node
application parity smoke; it creates the install, node bootstrap, and readiness
foundation that Slice 7 must consume.

## Requirements From Parent Plan

- Covers R1 by making every node start the same local daemon, gateway, DNS, and
  runtime-support roles.
- Covers R8 by adding a real install/bootstrap surface for clean Linux node
  roots.
- Prepares R9-R13 by making equal node bootstrap, role readiness, and restart
  observation available to the final parity smoke.
- Advances R14-R15 by keeping install, node state, role startup, and smoke
  harness responsibilities in separate files and tests.

## Current Evidence

- `MVP/node/src/main.rs` exposes product commands for `init`, `join`, `daemon`,
  `gateway`, `dns`, `deploy`, and `acme-issue`, but there is no single
  install/bootstrap command that lays out a runnable node root.
- `MVP/node/src/config.rs` and `MVP/node/src/state.rs` centralize node paths and
  identity material, including p2panda, WireGuard, serving snapshots, runtime
  dirs, and container subnet derivation.
- `MVP/e2e/src/three_server_harness.rs` already drives the `mvp-node` binary as
  a subprocess, but it resolves a developer build output and starts only the
  roles each scenario asks for.
- `scripts/build-install-payload.sh`, `scripts/bootstrap-linux.sh`, and
  `crates/ployz-install` are proven source material for install payload shape,
  but they currently target the non-MVP binaries and system services.

## Design Decisions

### Installed Binary Proof Is Required For This Slice

The acceptance path must run an installed `mvp-node` binary from a clean install
root. It may install into a temp prefix for the non-privileged local harness,
but the command path must be the installed path, not a library call or an
implicit target/debug lookup.

### Bootstrap Is A Product Surface

Bootstrap should be exposed as an MVP product command or script with structured
output. The harness can call it first, but it must be usable outside the harness
with explicit inputs: install root, node state dir, node id, island, role socket
dir, and runtime mode.

### Role Readiness Must Be Separated By Audience

Status output must distinguish initialized identity, daemon/control readiness,
gateway readiness, DNS readiness, WireGuard applied state, runtime adoption,
and ACME/account readiness where available. A failed role should be visible to
the operator and to the smoke runner without parsing generic logs.

### Linux Privileged Preconditions Stay Explicit

Docker, WireGuard, and forwarding checks may be gated when the local developer
machine lacks root or Linux kernel support. The slice must still prove the
non-privileged install/binary/bootstrap path and record privileged blockers as
explicit readiness states for Slice 7.

### No Hidden Supervisor Loop In MVP State

This slice may start roles for the smoke harness, but it must not add a
background reconciler that silently restarts or mutates durable truth. If a role
is not ready, status reports that fact; the operator or harness decides the
next command.

## Implementation Units

### U1. MVP Install Payload And Binary Resolution

**Goal:** produce a clean installed `mvp-node` binary path that the smoke
harness can use without target-dir assumptions.

**Requirements:** R8, R14, R15.

**Dependencies:** none.

**Files:**

- `MVP/node/Cargo.toml`
- `MVP/e2e/src/three_server_harness.rs`
- `MVP/scripts/three-server-smoke.sh`
- `scripts/build-install-payload.sh` or a focused MVP-local install script if
  extending the repository payload would mix MVP and non-MVP ownership.

**Approach:**

- Add a minimal MVP install payload path that copies the built `mvp-node`
  binary into a temp install root with a stable `bin/mvp-node` location.
- Keep the existing non-MVP install payload intact unless the smallest clean
  change is to make it optionally include MVP binaries.
- Teach the product harness to accept an installed binary path and record that
  path in scenario evidence.
- Keep fallback developer binary resolution only for lower-level scenarios that
  are not proving install.

**Patterns to follow:**

- `scripts/build-install-payload.sh` for payload freshness and stable binary
  layout.
- `MVP/e2e/src/three_server_harness.rs` for product-command recording and
  subprocess timeouts.

**Test scenarios:**

- Given a clean temp install root, the install step places an executable
  `bin/mvp-node` and running `bin/mvp-node --help` succeeds.
- Given an installed binary path, `ProductHarness` records commands with that
  path and does not resolve `target/debug/mvp-node`.
- Given a missing installed binary, the install-smoke path fails with a clear
  setup error before bootstrapping nodes.

**Verification:** an installed-binary harness test proves command execution
from the install root and emits the installed path in scenario artifacts.

### U2. Node Bootstrap Command

**Goal:** add a product bootstrap surface that initializes a node root and all
local runtime directories needed by daemon, gateway, DNS, WireGuard, runtime,
and ACME roles.

**Requirements:** R1, R8, R13, R15.

**Dependencies:** U1.

**Files:**

- `MVP/node/src/main.rs`
- `MVP/node/src/config.rs`
- `MVP/node/src/state.rs`
- `MVP/node/src/error.rs`
- `MVP/node/tests/product_bootstrap.rs`

**Approach:**

- Add a `bootstrap` command or equivalent product entrypoint that wraps
  `init_node` for clean nodes and verifies required node-local directories.
- Return structured JSON output containing node id, island, state dir,
  fact/projection paths, runtime dir, WireGuard identity status, serving
  snapshot paths, and role socket defaults.
- Keep join/admission as explicit commands; bootstrap should not silently admit
  peers or mutate another node's authority.
- Make reruns idempotent only for the same initialized node identity. A
  conflicting node id or island should fail loudly.

**Patterns to follow:**

- Existing `init`, `join`, `status`, and node-state path generation in
  `MVP/node/src/main.rs` and `MVP/node/src/state.rs`.
- Structured command output pattern from `deploy-status` and `acme-issue`.

**Test scenarios:**

- Clean state dir bootstrap writes all expected path roots and identity
  material.
- Re-running bootstrap with the same node id reports the existing node without
  rotating keys.
- Re-running bootstrap with a different node id fails and leaves the original
  state intact.
- Bootstrap output includes enough role paths for the harness to start daemon,
  gateway, and DNS without hard-coded state edits.

**Verification:** `mvp-node bootstrap` can replace `init` in the three-node
harness for clean node dirs.

### U3. Equal-Node Admission Bootstrap

**Goal:** bootstrap `node-a`, `node-b`, and `node-c` as peers using product
invite/admission commands only.

**Requirements:** R1, R8, R9, R14.

**Dependencies:** U2.

**Files:**

- `MVP/e2e/src/three_server_harness.rs`
- `MVP/e2e/src/three_server_product_contract.rs`
- `MVP/node/tests/product_bootstrap.rs`

**Approach:**

- Use the installed binary to bootstrap `node-a` as the first node.
- Use existing invite/join/admission/admit product commands to initialize
  `node-b` and `node-c`.
- Run the daemon/control role long enough on each node to import membership and
  expose node-agent readiness.
- Record the bootstrap command transcript as structured E2E evidence.

**Patterns to follow:**

- `MVP/e2e/src/membership_wireguard_contract.rs` for admission facts and
  membership expectations.
- Existing `ProductHarness` command recording and timeout behavior.

**Test scenarios:**

- Three clean node dirs bootstrap into the same island without manual fact
  writes.
- `node-b` and `node-c` learn the founder and each other through product
  admission flow.
- Admission failure leaves the joining node initialized but not treated as a
  full peer by the founder.

**Verification:** a focused E2E contract reports three admitted equal nodes and
the command transcript contains only product commands.

### U4. Role Startup And Readiness Matrix

**Goal:** start daemon, gateway, DNS, and runtime-support roles for every
bootstrapped node and expose role-specific readiness.

**Requirements:** R1, R2, R3, R6, R8, R13, R14.

**Dependencies:** U3.

**Files:**

- `MVP/node/src/main.rs`
- `MVP/node/src/serving.rs`
- `MVP/node/src/membership/daemon_control.rs`
- `MVP/e2e/src/three_server_harness.rs`
- `MVP/node/tests/product_serving_roles.rs`
- `MVP/node/tests/product_bootstrap.rs`

**Approach:**

- Add or harden a product status/readiness command that reports one row per
  local role: daemon, gateway, DNS, runtime, WireGuard, projection, and ACME.
- Preserve existing serving role control sockets, but make the harness discover
  and assert their readiness from bootstrap/status output.
- For privileged Linux roles, report preflight status separately from readiness
  so the final smoke can distinguish "not supported here" from "role failed."
- Keep role startup explicit in the harness or command surface; do not add a
  reconciler that keeps restarting roles.

**Patterns to follow:**

- `MVP/node/src/serving.rs` control socket status model.
- `MVP/node/src/membership/daemon_control.rs` daemon status behavior.
- `MVP/e2e/src/container_overlay_dns_smoke.rs` for explicit privileged
  preflight reporting.

**Test scenarios:**

- After bootstrap and role start, readiness reports daemon, gateway, and DNS
  ready for each node.
- If the gateway process is stopped, readiness reports gateway unavailable
  while daemon/DNS status remains independently visible.
- On non-Linux or non-root runs, WireGuard/Docker privileged checks report a
  gated preflight status instead of passing falsely.
- Daemon restart preserves gateway/DNS process readiness and reports runtime
  adoption state after daemon comes back.

**Verification:** a harness test boots three nodes, starts local roles on each,
and asserts the readiness matrix without reading private state files.

### U5. Installed-Binary Bootstrap Smoke

**Goal:** create the committed Slice 6 acceptance scenario that proves install,
bootstrap, equal-node role readiness, and restart observation.

**Requirements:** R1, R8, R9, R13, R14.

**Dependencies:** U1, U2, U3, U4.

**Files:**

- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/three_server_harness.rs`
- `MVP/e2e/src/installed_bootstrap_contract.rs`
- `MVP/scripts/three-server-smoke.sh`

**Approach:**

- Add an E2E scenario such as `installed-bootstrap-contract`.
- Build or install the MVP binary into a clean temp install root, then run all
  node commands via that installed path.
- Bootstrap three clean node dirs, admit/join peers, start roles on all nodes,
  collect readiness, restart one daemon, and collect readiness again.
- Write a JSON report with install path, command transcript, node ids, role
  readiness matrix, privileged preflight statuses, and restart evidence.

**Patterns to follow:**

- Existing scenario registration in `MVP/e2e/src/main.rs`.
- `MVP/e2e/src/pebble_acme_https_contract.rs` for external process and
  artifact-style proof.
- `MVP/e2e/src/three_server_product_contract.rs` for product-command smoke
  style.

**Test scenarios:**

- Happy path passes with three clean nodes and no hand-written state.
- The scenario fails if any command uses a developer target binary instead of
  the installed binary path.
- The scenario fails if peer bootstrap requires direct fact-store edits.
- Restarting one daemon does not stop the local gateway/DNS role processes.

**Verification:** the new E2E scenario passes locally in non-privileged mode for
install/bootstrap/role readiness and records any Linux privileged blockers for
Slice 7.

### U6. Slice Gate, Review, Commit, And Next-Slice Planning

**Goal:** finish Slice 6 with durable evidence and prepare Slice 7 from the
actual install/bootstrap surface that landed.

**Requirements:** R14, R15.

**Dependencies:** U5.

**Files:**

- `docs/plans/2026-05-20-002-feat-mvp-data-plane-parity.md`
- `docs/plans/2026-05-20-008-feat-mvp-real-install-bootstrap-slice.md`
- `docs/plans/2026-05-20-009-feat-mvp-three-node-parity-smoke-slice.md`

**Approach:**

- Run the slice in LFG order: plan, implement, review, test, persist fixes,
  commit, push.
- Keep review effort proportional: lightweight self-review for parser/status
  wiring, stronger review for bootstrap state mutation and installed-binary E2E
  behavior.
- Update this plan with evidence and update the parent plan status.
- Create the focused Slice 7 plan only after Slice 6 lands, so the final parity
  smoke consumes the real bootstrap commands and readiness schema.

**Patterns to follow:**

- The completed Slice 5 plan's U6A-U6D evidence style.
- Parent plan requirement mapping and final gate format.

**Test scenarios:**

- No extra product behavior; this unit verifies documentation and execution
  hygiene.

**Verification:** Slice 6 has a pushed commit, recorded test evidence, and a
Slice 7 plan that explicitly starts from the installed-bootstrap scenario.

## Verification Gates

Run the focused gates that match the implementation:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node --test product_serving_roles`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- installed-bootstrap-contract`
- `MVP/scripts/three-server-smoke.sh` once it uses the installed binary path
- Any privileged Linux/Docker/WireGuard preflight smoke enabled by the host,
  with skips recorded as explicit blockers rather than accepted parity proof

## Acceptance Checklist

- A clean install root contains an executable `bin/mvp-node`.
- The Slice 6 smoke runs through the installed binary path.
- Three clean node dirs bootstrap without hand-edited state or direct fact-store
  writes.
- Node identity material covers p2panda, WireGuard, runtime, serving snapshots,
  and ACME account storage paths.
- Every node reports daemon, gateway, DNS, runtime, WireGuard, projection, and
  ACME readiness or explicit gated preflight status.
- Restarting one daemon does not stop already-running gateway/DNS roles.
- The next Slice 7 plan exists and consumes the final Slice 6 command/status
  surface.

## Explicit Deferrals

- Full workload placement for `web`, `api`, and `echo` remains Slice 7.
- Cross-machine Docker/WireGuard service traffic remains Slice 7 unless the
  implementation naturally completes the existing privileged Slice 3 smoke.
- Public CA issuance and non-Linux install are out of scope.
- Systemd service installation for MVP roles is optional in this slice unless it
  is the smallest clean way to prove installed-binary execution on the target
  Linux runner.
