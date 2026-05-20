---
title: MVP Data-Plane Parity Slice 7 Three-Node Parity Smoke
status: active
created: 2026-05-20
type: feature
parent_plan: docs/plans/2026-05-20-002-feat-mvp-data-plane-parity.md
---

# MVP Data-Plane Parity Slice 7 Three-Node Parity Smoke

## Problem Frame

Slice 6 proved that the MVP can install `mvp-node` into a clean binary path,
bootstrap three equal node roots through product commands, start local gateway
and DNS roles on each node, and restart a daemon without stopping those roles.
Slice 7 must turn that bootstrap proof into the final data-plane parity smoke:
real workload placement, cross-node gateway behavior, service DNS, HTTPS,
Pebble ACME, update/drain, and restart survival.

The smoke must consume the installed-bootstrap surface that now exists. It
should not reintroduce in-process helpers, direct fact-store edits, or a special
gateway node.

## Requirements From Parent Plan

- Covers R1 by running the same daemon, gateway, DNS, runtime, WireGuard, and
  ACME-capable stack on `node-a`, `node-b`, and `node-c`.
- Covers R4-R5 by proving gateway-to-backend and container-to-container service
  DNS paths across nodes.
- Covers R6-R7 by validating HTTP, HTTPS, HTTP-01, and Pebble-issued
  certificates through Pingora.
- Covers R9-R13 by placing `web`, `api`, and `echo` on different nodes,
  updating `api`, draining the old backend, and restarting one daemon while the
  data plane keeps serving.
- Covers R14-R15 by keeping the smoke explicit, evidence-rich, and split before
  mixed-responsibility code grows.

## Current Evidence To Build From

- `installed-bootstrap-contract` installs and runs `bin/mvp-node`, bootstraps
  `node-a`, `node-b`, and `node-c`, starts gateway/DNS roles on all nodes, and
  records daemon restart evidence.
- `pebble-acme-https-contract` proves product ACME issuance against Pebble and
  Pingora TLS on a local product gateway.
- `three-server-product` proves product deploy/update/drain with process
  runtime and daemon control, but it still uses loopback/process assumptions.
- `container-overlay-dns-smoke` has preflight and scenario registration, but
  the real two-node overlay execution remains pending.
- Slice 3 privileged checks remain explicit host blockers when Docker,
  WireGuard, or root capabilities are unavailable.

## Design Decisions

### Installed Bootstrap Is The Harness Entry Point

The final smoke starts by invoking the same install path and bootstrap commands
used by `installed-bootstrap-contract`. The smoke may share harness helpers, but
it must not bypass the installed binary or write node state directly.

### Privileged Data-Plane Proof Must Stay Binary

If the host can run Docker/WireGuard/eBPF, the smoke should execute the real
cross-node paths. If it cannot, the smoke must fail or skip with explicit
preflight evidence. Passing a fake overlay is not parity proof.

### Workload Behavior Is Scenario Evidence

The final report must record gateway URLs, DNS answers, certificate issuer/root
evidence, deployed revisions, old-backend drain evidence, daemon restart
evidence, and any privileged preflight blockers. Logs alone are not evidence.

### Keep Product Commands Command-Shaped

The smoke can call `bootstrap`, `daemon`, `gateway`, `dns`, `deploy`,
`acme-issue`, and status/control commands. It must not add a controller or
background loop to converge the cluster.

## Implementation Units

### U1. Parity Smoke Harness From Installed Bootstrap

**Goal:** create the Slice 7 scenario shell around the installed-bootstrap
setup and produce a report skeleton.

**Requirements:** R1, R8, R9, R14.

**Dependencies:** Slice 6.

**Files:**

- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/three_server_harness.rs`
- `MVP/e2e/src/three_node_parity_smoke.rs`
- `MVP/scripts/three-server-smoke.sh`

**Approach:**

- Add a `three-node-parity-smoke` scenario that starts from
  `ProductHarness::install`.
- Bootstrap `node-a`, `node-b`, and `node-c` through product commands only.
- Start daemon, gateway, and DNS roles for every node with per-node control
  sockets.
- Emit a report with installed binary path, command transcript, role readiness,
  and privileged preflight status.

**Test scenarios:**

- Scenario fails if any command uses a developer target binary instead of the
  installed path.
- Scenario fails if node state is hand-written outside product commands.
- Scenario records explicit privileged blockers on non-root/non-Linux hosts.

**Verification:** the scenario reaches the same readiness point as
`installed-bootstrap-contract` before workload actions begin.

### U2. Real Runtime And Overlay Placement

**Goal:** deploy `web`, `api`, and `echo` through real runtime/overlay paths
onto separate nodes.

**Status:** Docker runtime placement path is wired through product daemon
commands and the installed-binary smoke report. Local execution remains an
explicit host-blocked skip on the current macOS/non-root host.

**Requirements:** R3, R4, R9, R14.

**Dependencies:** U1.

**Files:**

- `MVP/e2e/src/three_node_parity_smoke.rs`
- `MVP/e2e/src/container_overlay_dns_smoke.rs`
- `MVP/e2e/src/three_server_harness.rs`
- `MVP/node/src/main.rs` if deploy flags need small product-surface wiring

**Approach:**

- Reuse Docker runtime flags and node daemon control deploy path.
- Place `web` on `node-a`, `api` on `node-b`, and `echo` on `node-c`.
- Ensure backend endpoints are overlay/container addresses in Docker mode.
- Keep process runtime available only for non-parity lower-level tests.

**Test scenarios:**

- `web` deploy returns active backend on `node-a` with a non-loopback Docker
  endpoint when privileged mode is enabled.
- `api` deploy returns active backend on `node-b`.
- `echo` deploy returns active backend on `node-c`.
- Missing Docker/WireGuard prerequisites produce explicit blocker evidence.

**Verification:** deployment report proves each service is placed on its
required node with real runtime backend evidence.

### U3. Equal-Node Gateway HTTP/HTTPS And ACME

**Goal:** verify every node gateway can serve routes for services placed on
other nodes, including Pebble-issued HTTPS.

**Status:** privileged U3 branch is wired into the installed-binary smoke. The
current host still records the Linux/root/tooling preflight blockers before
runtime placement, gateway HTTP, or Pebble HTTPS can execute.

**Requirements:** R6, R7, R10, R11, R14.

**Dependencies:** U2.

**Files:**

- `MVP/e2e/src/three_node_parity_smoke.rs`
- `MVP/e2e/src/pebble_acme_https_contract.rs`
- `MVP/e2e/src/three_server_harness.rs`
- `packaging/e2e/pebble/*`

**Approach:**

- Start Pebble using the existing local pattern.
- Issue certificates with `mvp-node acme-issue` through product gateway control.
- Verify `web` through gateways on `node-b` and `node-c`.
- Verify `api` through gateways on `node-a` and `node-c`.
- Use Pebble root validation, SNI, and Host headers in client checks.

**Test scenarios:**

- HTTP-01 readiness fails before `set_ready` if the challenge is not visible
  through a product gateway.
- HTTPS validation fails if Pingora does not serve the projected certificate.
- Gateways on non-owner nodes can proxy to remote service backends.

**Verification:** report includes Pebble directory/root evidence, certificate
hostnames, gateway addresses, and HTTPS response bodies.

### U4. Container-Facing Service DNS Client

**Goal:** run a one-shot client on `node-a` that resolves and curls `echo` on
`node-c` by service DNS over the overlay.

**Status:** privileged U4 branch is wired into the installed-binary smoke. The
current host still records the Linux/root/tooling preflight blockers before
runtime placement or container DNS can execute.

**Requirements:** R5, R12, R14.

**Dependencies:** U2.

**Files:**

- `MVP/e2e/src/three_node_parity_smoke.rs`
- `MVP/node/tests/container_service_dns.rs`
- `MVP/runtime/src/docker/backend.rs` if one-shot client support needs a narrow
  runtime helper

**Approach:**

- Reuse the Docker service-DNS smoke mechanics.
- Run the client through product/runtime surfaces, not a host curl shortcut.
- Record DNS answer and HTTP body from inside the client container.

**Test scenarios:**

- Client on `node-a` resolves `echo` to a container/overlay address.
- Client curl reaches `echo` on `node-c`.
- DNS failure and HTTP failure are reported separately.

**Verification:** report includes the service DNS name, DNS answer, and client
HTTP response body.

### U5. Update/Drain And Daemon Restart Survival

**Goal:** update `api` from v1 to v2, verify all gateways converge, verify old
backend drain, and restart one daemon without interrupting data-plane serving.

**Status:** privileged U5 branch is wired into the installed-binary smoke. The
smoke now emits revision-distinct container bodies, proves `api` returns
`rev-1` before update and `rev-2` after update on every gateway, records the
deploy response old-backend count plus cleanup status phases, reloads gateways
through product control sockets, restarts `node-b`'s daemon, and rechecks
gateway plus container-DNS responses. The current host still records the
Linux/root/tooling preflight blockers before U5 can execute for real.

**Requirements:** R11, R13, R14.

**Dependencies:** U3, U4.

**Files:**

- `MVP/e2e/src/three_node_parity_smoke.rs`
- `MVP/e2e/src/three_server_harness.rs`
- `MVP/node/src/deploy.rs` if deploy status evidence needs small exposure

**Approach:**

- Deploy `api` v1 to `node-b`, then deploy v2 with a new deploy id.
- Reload or observe gateways through product control sockets.
- Verify gateways on `node-a`, `node-b`, and `node-c` return v2.
- Assert old-backend drain/cleanup is recorded in deploy status.
- Restart one daemon and re-check gateway/DNS and container responses.

**Test scenarios:**

- All gateways return v1 before update and v2 after update.
- Old backend is marked drained/stopped after update.
- Restarting one daemon preserves gateway, DNS, and already-running containers.

**Verification:** report includes v1/v2 response bodies, deploy status phases,
old-backend drain evidence, and post-restart responses.

### U6. Final Gate And Requirement Audit

**Goal:** finish the top-level parity push with evidence mapped to R1-R14.

**Status:** complete for the local documentation gate. The parent plan now has
a requirement-by-requirement audit that separates local passing evidence from
the privileged Linux data-plane blocker and names the next required evidence
command before the parity push can be called complete.

**Requirements:** R14, R15.

**Dependencies:** U5.

**Files:**

- `docs/plans/2026-05-20-002-feat-mvp-data-plane-parity.md`
- `docs/plans/2026-05-20-009-feat-mvp-three-node-parity-smoke-slice.md`

**Approach:**

- Run the slice in LFG order: implement, review, test, commit, push.
- Keep residual privileged blockers explicit if the current host cannot execute
  Linux data-plane checks.
- Update the parent plan with a requirement-by-requirement completion audit.

**Test scenarios:**

- Documentation-only unit; no new runtime behavior.

**Verification:** final parent plan maps every R1-R14 requirement to a passing
command output, scenario report field, or explicit host blocker.

## Verification Gates

- U1 checkpoint:
  - `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- three-node-parity-smoke`
  - `MVP/scripts/three-server-smoke.sh three-node-parity-smoke`
  - Evidence: both commands reach `stage=installed-bootstrap-readiness`, run
    commands through `target/mvp-e2e/three-node-parity-smoke/install/bin/mvp-node`,
    start daemon/gateway/DNS roles on `node-a`, `node-b`, and `node-c`, and
    record local host blockers: non-Linux, non-root, missing `ip`,
    `iptables`, and `ployz-bpfctl`.
- U2 checkpoint:
  - `cargo check --manifest-path MVP/Cargo.toml -p mvp-node`
  - `cargo check --manifest-path MVP/Cargo.toml -p mvp-node --features docker-runtime`
  - `cargo check --manifest-path MVP/Cargo.toml -p mvp-node --features docker-runtime,linux-wireguard`
  - `cargo test --manifest-path MVP/Cargo.toml -p mvp-node daemon_args_accept_docker_runtime_surface -- --nocapture`
  - `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- three-node-parity-smoke`
  - `MVP/scripts/three-server-smoke.sh three-node-parity-smoke`
  - Evidence: `mvp-node daemon` now accepts `--runtime docker --image
    busybox:latest --service-port 8080 --container-command <cmd>`, the harness
    installs a Docker-runtime and Linux-WireGuard-enabled `mvp-node`, and the
    smoke contains a privileged branch that starts the Linux WireGuard backend,
    deploys `web`, `api`, and `echo` to `node-a`, `node-b`, and `node-c`, and
    rejects loopback Docker backends. On the current host that branch is blocked
    by the recorded Linux/root/tooling preflight.
- U3 checkpoint:
  - `cargo check --manifest-path MVP/Cargo.toml -p mvp-e2e`
  - `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- three-node-parity-smoke`
  - `MVP/scripts/three-server-smoke.sh three-node-parity-smoke`
  - `TMPDIR=/tmp cargo test --manifest-path MVP/Cargo.toml -p mvp-e2e`
  - Evidence: the smoke report now carries `gateway_http_checks` and
    `acme_https_checks`. In privileged mode it starts TLS listeners on every
    gateway, verifies `web` through `node-b` and `node-c`, verifies `api`
    through `node-a` and `node-c`, starts Pebble, issues certificates through
    installed `mvp-node acme-issue`, and validates HTTPS with Pebble's issued
    root. On the current host those checks remain blocked by the recorded
    Linux/root/tooling preflight, so local reports keep both arrays empty.
- U4 checkpoint:
  - `cargo check --manifest-path MVP/Cargo.toml -p mvp-e2e`
  - `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- three-node-parity-smoke`
  - `MVP/scripts/three-server-smoke.sh three-node-parity-smoke`
  - `TMPDIR=/tmp cargo test --manifest-path MVP/Cargo.toml -p mvp-e2e`
  - Evidence: the smoke report now carries `container_dns_checks`. In
    privileged mode DNS roles bind to each node's Docker bridge gateway IP,
    the client runs in a one-shot BusyBox container on `node-a`'s Docker
    network with node-local DNS configured, `nslookup` must return a
    `10.210.*` service answer for `echo.service.example.test`, and `wget`
    must return the `echo` HTTP body. On the current host this remains blocked
    by the recorded Linux/root/tooling preflight, so local reports keep the
    array empty.
- U5 checkpoint:
  - `cargo check --manifest-path MVP/Cargo.toml -p mvp-e2e`
  - `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- three-node-parity-smoke`
  - `MVP/scripts/three-server-smoke.sh three-node-parity-smoke`
  - `TMPDIR=/tmp cargo test --manifest-path MVP/Cargo.toml -p mvp-runtime`
  - `TMPDIR=/tmp cargo test --manifest-path MVP/Cargo.toml -p mvp-e2e`
  - Evidence: Docker runtime containers receive `PLOYZ_SERVICE`,
    `PLOYZ_REVISION`, `PLOYZ_INSTANCE_ID`, and `PLOYZ_NODE` env vars so the
    smoke can distinguish `api` v1 from v2 by response body. The privileged
    smoke report now carries `update_drain_checks` and `daemon_restart_checks`.
    In privileged mode it verifies `ok-api-rev-1`, deploys `deploy-api-v2`,
    asserts nonzero old-backend drain count and `cleanup_done` deploy status,
    reloads gateways, verifies `ok-api-rev-2` through `node-a`, `node-b`, and
    `node-c`, restarts `node-b`'s daemon, then rechecks gateway and
    container-DNS responses. On the current host this remains blocked by the
    recorded Linux/root/tooling preflight, so local reports keep both arrays
    empty.
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- three-node-parity-smoke`
- `MVP/scripts/three-server-smoke.sh three-node-parity-smoke`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-mesh --features linux-wireguard`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-serving`
- `cargo test --manifest-path MVP/Cargo.toml --workspace`

Residual local Docker verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-runtime --features docker`
  was attempted on 2026-05-20. It passed runtime unit tests,
  `docker_bridge_network`, `docker_runtime_starts_lists_adopts_drains_and_stops_container`,
  and `docker_runtime_removes_new_container_when_readiness_fails`, then failed
  `docker_runtime_adopts_same_spec_and_recreates_changed_revision` with
  `ReadinessTimeout` on `10.210.91.*:8080`. Rerunning that single test after
  removing its stale test network reproduced the same readiness timeout on the
  current Docker host. This is tracked as a host/runtime integration residual,
  not as passing U2 evidence.

## Acceptance Checklist

- [x] Smoke runs through installed `bin/mvp-node`.
- [x] `node-a`, `node-b`, and `node-c` are equal gateway/DNS/daemon nodes.
- [ ] `web`, `api`, and `echo` are placed on separate required nodes. Product
  path is wired; passing evidence still requires a privileged Linux host.
- [ ] Gateways on non-owner nodes serve remote services. Product path is wired;
  passing evidence still requires a privileged Linux host.
- [ ] Pebble ACME issues certificates and HTTPS validates with Pebble root.
  Product path is wired; passing evidence still requires a privileged Linux
  host.
- [ ] Container client resolves and curls `echo` by service DNS. Product path
  is wired; passing evidence still requires a privileged Linux host.
- [ ] `api` update drains old backend and all gateways converge to v2. Product
  path is wired; passing evidence still requires a privileged Linux host.
- [ ] Daemon restart does not stop gateway, DNS, or already-running containers.
  Product path is wired; passing evidence still requires a privileged Linux
  host.
- [x] Parent plan final gate has a requirement audit for R1-R14.

## Next Slice 7 Sub-Slice

The next implementation pass should run the current smoke on a privileged
Linux host and fix any placement, gateway, Pebble, container DNS,
update/drain, or daemon restart failures surfaced there. U6 has recorded the
audit, but U2-U5 should not be marked passing until that privileged evidence
exists.

## Explicit Deferrals

- ZFS remains out of scope until this parity smoke passes.
- Public Let's Encrypt issuance remains out of scope.
- BSD/Darwin install remains out of scope.
- UI/UX polish beyond command/status evidence remains out of scope.
