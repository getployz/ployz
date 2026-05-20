---
title: MVP Data-Plane Parity Push
status: active
created: 2026-05-20
type: feature
origin: user data-plane completion goal
---

# MVP Data-Plane Parity Push

## Problem Frame

The MVP now has a clean enough control-plane shape to stop proving product
behavior with placeholders. The next push is to make the Linux data plane real
end to end while preserving the current architecture: explicit commands,
backend traits below orchestration, and equal peer nodes.

The target is not a new platform layer or a compatibility shim. It is parity
between the MVP command surface and real Linux substrates:

- WireGuard networking is real.
- Workloads run as real containers.
- Container networking and service DNS work across machines.
- Every node runs the same gateway/DNS/data-plane roles.
- Gateway HTTP/HTTPS is backed by Pingora.
- ACME issuance uses Pebble/challtestsrv with `instant-acme`, not a toy fake.
- A three-node parity smoke proves install, deploy, overlay, DNS, HTTPS, ACME,
  update/drain, and daemon restart survival.

This plan follows `VISION.md`: the data plane must outlive the daemon, every
node is a peer, and operational changes remain command-shaped rather than
hidden in a reconciler.

## Scope

In scope:

- Linux-only real data-plane backends for the MVP.
- Docker/container runtime execution behind the existing runtime boundary.
- Kernel/userspace WireGuard application behind the existing mesh boundary.
- Container-to-container overlay networking and service DNS for deployed
  services.
- Pingora-backed HTTP/HTTPS gateway on every node.
- Pebble-backed ACME HTTP-01 issuance through `instant-acme`.
- A real install/bootstrap flow usable by the parity smoke.
- A cross-machine smoke with three equal nodes: `node-a`, `node-b`, `node-c`.
- Additional slices when a subsystem needs its own planning/verification loop
  to stay production-shaped.

Out of scope until the parity smoke passes:

- ZFS.
- BSD/Darwin support.
- Public CA issuance against Let's Encrypt.
- Replacing the control-plane model or adding background reconcilers.
- Building a general Kubernetes-style service abstraction.
- Large UX polish beyond the install/deploy/status surfaces needed by the
  parity smoke.

## Requirements

- R1: Every node runs the same data-plane stack: daemon, WireGuard, container
  runtime integration, service DNS, Pingora gateway, and ACME challenge serving.
- R2: WireGuard application uses real Linux interfaces/routes and is restart
  adoptable; daemon restart must not tear down the overlay.
- R3: Workloads run through a real container runtime backend, not
  `ProcessRuntime`.
- R4: Service backends are reachable over the overlay, including
  gateway-to-backend and container-to-container paths across different nodes.
- R5: Service DNS resolves deployed services from inside containers across the
  overlay.
- R6: Pingora serves HTTP routes, HTTPS routes, and HTTP-01 challenge paths from
  the existing projection/snapshot state model.
- R7: ACME uses Pebble/challtestsrv and `instant-acme`; issued certificates
  validate against Pebble's root.
- R8: The install flow can bootstrap a target Linux node into the MVP data
  plane without hand-edited state.
- R9: The parity smoke places `web` on `node-a`, `api` on `node-b`, and `echo`
  on `node-c`; all nodes are equal gateway nodes.
- R10: The smoke verifies `web` through gateways on `node-b` and `node-c`.
- R11: The smoke verifies `api` through gateways on `node-a` and `node-c`,
  updates `api` from v1 to v2, and verifies all gateways converge to v2 with
  the old backend drained.
- R12: The smoke runs a one-shot `client` on `node-a` that resolves and curls
  `echo` on `node-c` by service DNS over the overlay.
- R13: Restarting the daemon on one node leaves its local gateway and already
  running containers serving.
- R14: Verification stays green with focused tests per slice and a final
  workspace plus parity smoke gate.
- R15: New production files stay small enough to review and own one concept.
  If a file approaches 1,000 LOC or mixes runtime, networking, serving, ACME,
  install, and orchestration responsibilities, split by concept before adding
  more behavior.

## Current Architecture To Preserve

- `MVP/runtime/src/process.rs` is the placeholder runtime. It persists local
  instance metadata and starts a static HTTP child process. Replace the backend
  behind the runtime concept; do not make deploy orchestration Docker-specific.
- `MVP/mesh/src/wireguard.rs` already has `WireGuardBackend`,
  `WireGuardPeerPlan`, and `plan_full_mesh`. The real Linux backend should
  implement that boundary and keep planning pure.
- `MVP/mesh/src/linux.rs` currently has `HostNetworkBackend`, which only
  validates host endpoints and writes a snapshot. The real data-plane push must
  supersede this with overlay/backend reachability rather than extend it into a
  second network model.
- `MVP/serving/src/http_gateway.rs` is a hyper-based gateway over
  `WireServingState`. Pingora should consume the same serving state boundary.
- `MVP/serving/src/dns_server.rs` already uses hickory protocol primitives for
  DNS answers from serving state. Service DNS should add a container-facing
  resolver surface without breaking the public route DNS semantics.
- `MVP/acme` and `MVP/acme-command` already model HTTP-01 challenge ownership,
  presentation, projection, and clearing. The missing piece is the real ACME
  order/account/finalize loop using Pebble through `instant-acme`.
- `crates/ployz-e2e/src/scenarios/deploy_http_acme_gateway_smoke.rs` and
  `crates/ployz-e2e/src/runner.rs::start_pebble_for_http01` are the reference
  pattern for Pebble/challtestsrv. MVP should port that pattern instead of
  inventing a fake provider.
- `crates/ployz-cert-backends/src/instant_acme_issuer.rs` is the reference for
  `instant-acme` account/order/challenge/finalize behavior.
- Existing non-MVP networking, eBPF, runtime, gateway, and install code should
  be treated as preferred source material for complex substrate mechanics where
  it fits. Port proven pieces behind the MVP trait boundaries instead of
  reimplementing difficult Linux behavior from scratch or importing old
  orchestration types upward.

## Decisions

### Keep backend-specific code below existing trait boundaries

`mvp-node` may compose real backends, but command logic in `mvp-deploy`,
`mvp-routing`, `mvp-machine`, and `mvp-environment` must not import Docker,
WireGuard system tooling, Pingora, or Pebble directly.

### Prefer porting proven substrate mechanics

For complex networking, eBPF, container runtime, gateway, ACME, and install
work, first look for equivalent working code in the pre-existing codebase. If
the existing implementation already solved the Linux substrate problem, port
the mechanic into the MVP owner crate behind the existing boundary. Do not
clone old architecture wholesale, add compatibility shims, or let old backend
types leak into command-domain crates.

### Use Pebble, not a hand-rolled fake ACME provider

The test CA should be a real ACME server. The MVP parity smoke should run
Pebble and challtestsrv, configure the MVP with Pebble's directory/root, issue
through `instant-acme`, and validate HTTPS with Pebble's root certificate.

### Equal-node gateway behavior is part of the product

There is no special gateway node. Every node runs the same serving stack and
can route to service backends on other nodes through the overlay. The parity
smoke must query multiple node-local gateways for the same route.

### Service DNS is container-facing, not a second control-plane truth source

Service DNS should resolve from projected service/backend state and local
runtime/network metadata. It must not become a mutable service registry that
silently rewrites cluster truth.

### Install is a product surface, not just E2E shell glue

The parity smoke can be the first caller, but install/bootstrap must leave a
real command or script surface that a Linux operator can run outside the test
harness.

### Slice count can grow to protect quality

The original goal asked for 5-7 planned slices as a scoping signal, not a hard
limit. If implementation research shows that Docker runtime, network
attachment, service DNS, Pingora, ACME issuance, install, or smoke harness work
would become a mixed-responsibility patch, split the work into additional
slice plans before coding.

## Implementation Slices

Execution workflow:

- Keep this document as the single top-level plan and scope boundary.
- Before implementing each slice, generate a focused slice plan that names the
  exact files, trait decisions, tests, and acceptance evidence for that slice.
- Split a slice before implementation if it would create a god module, cross a
  responsibility boundary, or require unrelated test gates to pass together.
- Execute slices in LFG-style autonomous loops: plan, implement, review, test,
  fix failures, commit, and push regularly.
- Do not create PRs as part of the slice loop unless explicitly requested.
- Do not start a later slice until the current slice has a committed,
  pushed checkpoint and its slice-specific evidence is recorded.

Quality gates for every slice:

- The slice plan must name the owner concept for each new production module.
- No handler or runtime file may own orchestration, substrate mutation,
  persistence, and presentation at once.
- No Docker, WireGuard, Pingora, Pebble, or install detail may leak into
  command-domain crates.
- Before commit, run a small LOC/responsibility audit on changed production
  files and split obvious mixed concepts immediately.

### Slice 1: Real Runtime Backend Boundary

Status: complete. Slice plan:
`docs/plans/2026-05-20-003-feat-mvp-runtime-backend-slice.md`.
Key checkpoints: `d95f5da8`, `4b399069`, `bd119e9b`.

Goal: replace the placeholder process runtime with a real container runtime
backend while keeping deploy orchestration runtime-agnostic.

Primary files:

- `MVP/runtime/src/lib.rs`
- `MVP/runtime/src/model.rs`
- `MVP/runtime/src/process.rs`
- `MVP/runtime/src/error.rs`
- `MVP/node/src/node_agent.rs`
- `MVP/node/src/deploy.rs`
- `MVP/e2e/src/three_server_harness.rs`

Work:

- Introduce a `RuntimeBackend` trait for prepare/start/drain/stop/list/adopt
  operations currently provided concretely by `ProcessRuntime`.
- Keep `ProcessRuntime` as a fixture/runtime backend for lower-level tests.
- Add a Docker-backed Linux runtime that starts containers with stable labels,
  service identity, revision, node identity, and network attachment metadata.
- Persist enough runtime metadata for daemon restart adoption.
- Return typed backend endpoints that later slices can map to overlay
  addresses rather than loopback ports.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-runtime`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node node_agent`
- A focused product deploy test proving the runtime trait can swap process and
  Docker implementations without changing deploy coordination.

Acceptance:

- Deploy code depends on the runtime trait, not Docker types.
- Docker containers survive daemon restart and are listed/adopted by the
  runtime backend.

### Slice 2: Real WireGuard Backend And Overlay Adoption

Status: complete except privileged smoke execution in this non-root
environment. Slice plan:
`docs/plans/2026-05-20-004-feat-mvp-wireguard-backend-slice.md`.
Key checkpoints: `d2093145`, `1cedd947`, `64e11cc1`, `b07edcc8`,
`d784899b`.

Goal: make the existing mesh plan apply to real Linux WireGuard state and keep
it alive independently of the daemon.

Primary files:

- `MVP/mesh/src/wireguard.rs`
- `MVP/mesh/src/linux.rs`
- `MVP/mesh/src/error.rs`
- `MVP/node/src/membership.rs`
- `MVP/node/src/membership/daemon_runtime.rs`
- `MVP/e2e/src/membership_wireguard_contract.rs`

Work:

- Add a Linux `WireGuardBackend` implementation that creates or adopts the
  interface, sets the local overlay address, installs peer allowed IPs, and
  records the applied snapshot.
- Generate or load real WireGuard private/public key material during node init
  and join.
- Ensure daemon startup adopts an existing interface instead of recreating it.
- Add bounded operation timeouts and structured failures for system commands or
  netlink calls.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-mesh`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node membership`
- A Linux-gated E2E or integration test proving two nodes can ping/curl over
  overlay IPs after daemon restart.

Acceptance:

- Real WireGuard state matches the projected full-mesh plan.
- Killing/restarting the daemon does not remove the interface or peers.

### Slice 3: Container Overlay Networking And Service DNS

Status: complete for local Docker/service-DNS behavior; privileged two-node
overlay execution remains a carried blocker for Slice 7. Slice plan:
`docs/plans/2026-05-20-005-feat-mvp-container-overlay-dns-slice.md`.

Goal: connect runtime containers to the overlay and expose service DNS inside
containers across machines.

Primary files:

- `MVP/runtime/src/model.rs`
- `MVP/runtime/src/lib.rs`
- `MVP/mesh/src/linux.rs`
- `MVP/node/src/networking.rs`
- `MVP/serving/src/dns_server.rs`
- `MVP/serving/src/model.rs`
- `MVP/projection/src/model.rs`

Work:

- Define the runtime/network contract for service container addresses on the
  WireGuard overlay.
- Replace host-loopback endpoint publication with overlay-reachable backend
  endpoints.
- Add service DNS records for container-facing names, separate from public DNS
  records.
- Configure containers to use the node-local DNS server for service names.
- Preserve public DNS snapshot behavior for route hostnames.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-runtime -p mvp-mesh -p mvp-serving`
- An E2E contract that places `echo` on one node and runs `client` on another,
  resolving `echo` by service DNS and curling it over the overlay.

Acceptance:

- Container-to-container traffic crosses machines through the overlay.
- Service DNS resolution works from inside a container without public route
  DNS.

### Slice 4: Pingora HTTP/HTTPS Gateway On Every Node

Status: complete for HTTP/TLS gateway product wiring and local serving proof.
Slice plan:
`docs/plans/2026-05-20-006-feat-mvp-pingora-gateway-slice.md`.

Goal: replace the MVP hyper gateway with a Pingora-backed gateway that serves
HTTP, HTTPS, and ACME challenge paths from existing serving state.

Primary files:

- `MVP/serving/Cargo.toml`
- `MVP/serving/src/http_gateway.rs`
- `MVP/serving/src/wire.rs`
- `MVP/serving/src/actor.rs`
- `MVP/node/src/serving.rs`
- `MVP/e2e/src/wire_serving_contract.rs`

Reference files:

- `crates/ployz-gateway/src/snapshot.rs`
- `crates/ployz-gateway/src/routes.rs`
- `crates/ployz-gateway/Cargo.toml`

Work:

- Add a Pingora gateway backend that reads from `WireServingState`.
- Serve HTTP-01 challenge paths before route proxying.
- Add TLS certificate loading/reload hooks, initially wired to static/test
  cert state and then to ACME output in Slice 5.
- Keep last-good snapshot semantics: corrupt reloads do not replace serving
  state.
- Make `mvp-node gateway` start the Pingora backend by default on Linux while
  keeping the hyper backend available for focused tests if needed.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-serving`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- A wire-serving E2E proving two node-local gateways can proxy to a remote
  backend and keep serving after daemon shutdown.

Acceptance:

- Every node can run the gateway role.
- Gateway-to-backend traffic can cross the overlay.
- HTTP and HTTP-01 behavior remains compatible with existing projection facts.

### Slice 5: Pebble ACME Issuance With `instant-acme`

Status: complete for real Pebble issuance through the product gateway and
Pingora TLS. Slice plan:

- `docs/plans/2026-05-20-007-feat-mvp-pebble-acme-tls-slice.md`.

Goal: turn existing ACME challenge facts into real local ACME issuance through
Pebble.

Primary files:

- `MVP/acme/Cargo.toml`
- `MVP/acme/src/lib.rs`
- `MVP/acme-command/src/lib.rs`
- `MVP/acme-command/src/p2panda.rs`
- `MVP/node/src/deploy.rs`
- `MVP/node/src/serving.rs`
- `MVP/serving/src/model.rs`

Reference files:

- `packaging/e2e/pebble/pebble-config.json`
- `crates/ployz-e2e/src/runner.rs`
- `crates/ployz-e2e/src/scenarios/deploy_http_acme_gateway_smoke.rs`
- `crates/ployz-cert-backends/src/instant_acme_issuer.rs`

Work:

- Add an ACME issuer boundary in MVP that can start and finalize HTTP-01
  orders.
- Implement a Pebble-compatible `instant-acme` issuer.
- Reuse existing ACME claim/present/clear facts for challenge publication.
- Wait for challenge visibility through node-local gateway readiness before
  calling `set_ready`.
- Persist issued certificate material in node state or projected serving state
  so Pingora can reload it.
- Delete challenge facts for the completed order without clobbering newer
  in-flight tokens for the same hostname.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-acme -p mvp-acme-command`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-acme-http01-contract`
- A Pebble-backed MVP E2E that validates the issued cert against
  `pebble.minica.pem`.

Acceptance:

- Certificate issuance uses Pebble through the real ACME protocol.
- Pingora serves HTTPS using the issued certificate.

### Slice 6: Real Install Flow And Equal-Node Bootstrap

Status: complete for installed-binary bootstrap, equal-node admission, local
gateway/DNS readiness, and daemon restart observation. Slice plan:
`docs/plans/2026-05-20-008-feat-mvp-real-install-bootstrap-slice.md`.

Goal: make the parity smoke install and bootstrap nodes through product
surfaces instead of prewired local assumptions.

Primary files:

- `MVP/node/src/main.rs`
- `MVP/node/src/config.rs`
- `MVP/node/src/state.rs`
- `MVP/e2e/src/three_server_harness.rs`
- `MVP/scripts/three-server-smoke.sh`
- `packaging/e2e/e2e-node-entrypoint.sh`

Work:

- Add or harden an install/bootstrap command for Linux nodes that lays out
  state dirs, keys, runtime directories, serving role sockets, and data-plane
  prerequisites.
- Build and run the smoke through an installed `mvp-node` binary path, not
  through crate APIs or a hand-picked target binary from the developer shell.
- Make node init/join produce all identity material needed by WireGuard,
  p2panda, runtime, gateway, DNS, and ACME.
- Start daemon, gateway, DNS, and runtime support consistently on every node.
- Ensure status/readiness commands distinguish daemon readiness, gateway
  readiness, DNS readiness, WireGuard applied state, and runtime adoption.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- A harness test that bootstraps three equal nodes from clean directories and
  verifies each reports all local data-plane roles ready.

Acceptance:

- No smoke step hand-edits node state to make the data plane work.
- The slice has a committed installed-binary proof: build payload or install
  layout, install into a clean root, run the installed binary, bootstrap three
  clean node dirs, and verify role readiness from product commands.
- Every node can be independently restarted and report its local roles.

### Slice 7: Three-Node Data-Plane Parity Smoke

Status: active. U1 installed-binary harness shell is complete. U2 Docker
runtime placement path and U3 equal-node HTTP/HTTPS/Pebble paths are wired
behind explicit privileged preflight and are locally verified to preserve
blockers on the current macOS/non-root host. Slice plan:
`docs/plans/2026-05-20-009-feat-mvp-three-node-parity-smoke-slice.md`.

Goal: prove the full end-to-end product behavior on three equal Linux nodes.

Primary files:

- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/three_server_harness.rs`
- `MVP/e2e/src/three_server_product_contract.rs`
- `MVP/scripts/three-server-smoke.sh`
- `packaging/e2e/pebble/*`

Work:

- Create or upgrade the parity scenario with nodes `node-a`, `node-b`,
  `node-c`.
- Run the full data-plane stack on every node.
- Place `web` on `node-a` and verify `https://web.<test-domain>/` through
  gateways on `node-b` and `node-c`.
- Place `api` on `node-b` and verify
  `https://api.<test-domain>/health` through gateways on `node-a` and
  `node-c`.
- Place `echo` on `node-c`; run a one-shot `client` container on `node-a` that
  resolves and curls `echo` by service DNS.
- Update `api` from v1 to v2 and verify all gateways converge to v2 with old
  backend drain recorded.
- Restart the daemon on one node and verify its local gateway and already
  running containers keep serving.
- Validate ACME certificates against Pebble's root.

Tests:

- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- three-server-product`
  or the renamed parity scenario.
- `MVP/scripts/three-server-smoke.sh`
- `cargo test --manifest-path MVP/Cargo.toml --workspace`

Acceptance:

- The final report proves install bootstrap, equal-node gateway behavior,
  cross-machine overlay routing, service DNS, HTTPS, ACME issuance,
  update/drain, and restart survival.

## Final Gate

- All seven slices have landed or the plan has been intentionally revised with
  a narrower user-approved scope.
- `cargo test --manifest-path MVP/Cargo.toml --workspace` passes.
- `MVP/scripts/three-server-smoke.sh` passes with real runtime, real
  WireGuard, service DNS, Pingora, Pebble ACME, and equal-node routing.
- No ZFS or BSD/Darwin work has landed before the parity smoke passes.
- A completion audit maps every requirement R1-R14 to test output or concrete
  runtime evidence.

## Risks

- Real WireGuard may need privileged test environments. If CI cannot provide
  this, keep the Linux-gated smoke explicit and do not dilute it into a fake.
- Docker networking can easily become a second orchestration model. Keep the
  runtime backend responsible for container lifecycle and the mesh/network
  backend responsible for overlay reachability.
- Pingora TLS integration can balloon. The first version should serve projected
  routes and loaded certificates; advanced gateway policy belongs later.
- Pebble validates through real HTTP-01 behavior, so gateway, DNS, and ACME
  ordering failures will surface together. Preserve structured failure
  audiences so the smoke can identify which subsystem is not ready.
