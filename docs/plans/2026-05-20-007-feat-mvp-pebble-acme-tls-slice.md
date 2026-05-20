# MVP Data-Plane Parity Slice 5: Pebble ACME + Pingora HTTPS

## Goal

Turn the MVP ACME HTTP-01 facts into real certificate issuance and HTTPS serving.
The implementation should port the proven non-MVP Pebble/`instant-acme`
mechanics where they fit, but the MVP ownership model stays separate:

- `mvp-acme` owns issuer/domain concepts, account/order/certificate data, and
  protocol-facing errors.
- `mvp-acme-command` owns fact-backed challenge publication and cleanup.
- `mvp-projection` owns projected serving certificate state.
- `mvp-serving` owns gateway certificate lookup/reload and Pingora TLS serving.
- `mvp-node` wires the node-local issuer, serving actor, and gateway roles.

This is not a fake provider inside MVP. The acceptance ACME server is Pebble
plus challtestsrv from the existing E2E harness, and issuance uses the real ACME
protocol through `instant-acme`.

## Current State

Already present:

- Typed ACME host, token, key-authorization, challenge id, lease, present, and
  clear facts in `MVP/acme`.
- P2panda-backed claim/present/clear commands in `MVP/acme-command`.
- Gateway snapshots already carry active HTTP-01 challenges.
- Hyper and Pingora HTTP gateway engines both serve HTTP-01 before route lookup.
- The non-MVP codebase already has a working Pebble/challtestsrv E2E and
  `instant-acme` issuer implementation.
- `mvp-acme` now has the issuer boundary and `InstantAcmeIssuer`.
- `mvp-projection` now projects certificate activation facts into serving
  snapshots.
- `mvp-serving` now exposes projected serving certificates and Pingora can
  terminate TLS on an optional TLS listener using those certificates.

Missing:

- `mvp-node` does not yet expose a product command/role that runs issuance,
  persists ACME account credentials, writes certificate activation facts, and
  reloads serving snapshots.
- `mvp-node gateway` does not yet expose a TLS listener flag even though
  `mvp-serving` supports one.
- No MVP Pebble-backed E2E validates a cert against Pebble's root through the
  product binary.

## Reference Implementation To Port

Use these files as the source for proven mechanics:

- `crates/ployz-cert-backends/src/instant_acme_issuer.rs`
- `crates/ployz-orchestrator/src/certificates.rs`
- `crates/ployz-e2e/src/scenarios/deploy_http_acme_gateway_smoke.rs`
- `crates/ployz-e2e/src/runner.rs`
- `packaging/e2e/pebble/pebble-config.json`

Port concepts, not crate dependencies. In particular:

- Keep `instant-acme` and HTTP client/root-CA concerns out of projection and
  serving model crates.
- Preserve the order URL origin check before resuming/finalizing an order.
- Preserve token-scoped cleanup after successful finalization so a stale
  finalizer cannot delete a newer in-flight order's challenge.
- Preserve challenge visibility/readiness waiting before `set_ready`.
- Preserve account creation serialization per issuer URL.

## Implementation Units

### 1. MVP ACME Issuer Boundary

Add the missing issuer concept to `mvp-acme`:

- `AcmeIssuer` trait with `start_order` and `finalize_order`.
- `AcmeIssuerFactory` if node wiring needs issuer-url-bound construction.
- `AcmeIssuerConfig` with directory URL, contact email, and optional root CA.
- `StartedAcmeOrder`, `IssuedCertificate`, `AcmeAccountRecord`, and
  order/certificate error variants.
- Account coordination types for first-use account creation.

Acceptance:

- `mvp-acme` compiles without depending on node, serving, projection, or p2panda
  crates.
- Unit tests cover URL origin checks, malformed account credentials, and account
  coordination veto/failure.

### 2. `instant-acme` Pebble Adapter

Implement the real protocol adapter behind the issuer trait:

- Create/load ACME account credentials for the configured directory.
- Start HTTP-01 orders, extract token and key authorization.
- Delegate challenge publication through an MVP challenge publisher interface.
- Wait for local gateway visibility before calling `set_ready`.
- Finalize the order and return PEM fullchain/private key material.

Acceptance:

- The adapter uses `instant-acme`; no MVP fake issuer is used for acceptance.
- Pebble root CA path is honored for local E2E.
- Finalization refuses order URLs whose origin differs from the configured
  directory URL.

### 3. Fact-Backed Challenge Publication

Connect issuance to existing MVP ACME facts:

- Claim, present, and clear challenge facts through `PandaAcmeCommandAdapter`.
- Keep deletion scoped to the exact order tokens.
- Keep lease/visibility failures structured and visible.

Acceptance:

- Existing `p2panda-acme-http01-contract` still passes.
- New tests prove stale clear/finalize cannot remove a newer token for the same
  hostname.

### 4. Certificate Projection And Snapshot

Add certificate material to projected serving state:

- Represent certificate hostname, active version, fullchain PEM, private key PEM,
  not-before/not-after where available, and source order URL.
- Add fact payloads and reducer logic for certificate issued/active state.
- Write certificates into the serving snapshot set alongside gateway/DNS data.

Acceptance:

- Serving snapshots expose ACME challenges and active certificates without
  mixing issuance state into gateway routing.
- Snapshot revisions change when certificate material changes.

### 5. Pingora HTTPS Gateway

Teach the Pingora engine to serve HTTPS using projected certificate material:

- Add TLS listener/options without removing the existing HTTP listener.
- Load certs from the serving actor snapshot.
- Select certificates by SNI/host.
- Reload without restarting the gateway task when the serving snapshot updates.

Acceptance:

- Pingora serves HTTP-01 over HTTP and routes HTTPS using issued certs.
- Cert reload keeps existing route/proxy status surfaces intact.

### 6. Pebble E2E Smoke

Add an MVP E2E using the existing Pebble/challtestsrv pattern:

- Start Pebble and challtestsrv.
- Configure MVP ACME directory URL and Pebble root CA.
- Issue a cert for a test hostname.
- Curl HTTPS through Pingora with `--cacert pebble.minica.pem`.

Acceptance:

- The issued certificate validates against Pebble's root.
- The test fails if HTTP-01 is not visible through the node-local gateway.
- The test fails if Pingora is still HTTP-only.

## Remaining Execution Plan

This is the concrete plan to finish the slice after Unit 5.

### U6A. Product TLS Gateway Flag

**Goal:** make the already-implemented Pingora TLS listener reachable through
the `mvp-node gateway` command.

**Files:**

- `MVP/node/src/main.rs`
- `MVP/node/src/serving.rs`
- `MVP/node/tests/product_serving_roles.rs`
- `MVP/README.md`

**Approach:**

- Add `--tls-listen <addr>` to `mvp-node gateway`.
- Extend `ServingRoleOptions` with `tls_listen: Option<SocketAddr>`.
- Pass the TLS listener into `GatewayOptions::with_tls_listener` only for the
  gateway role.
- Include `tls_listen_addr` in `ServingRoleProcessStatus` so tests and operators
  can discover the bound TLS port when `127.0.0.1:0` is used.
- Keep DNS role unchanged; reject or ignore no TLS option there by construction
  because `--tls-listen` belongs only to the gateway command parser.

**Test Scenarios:**

- Gateway role started with `--tls-listen 127.0.0.1:0` reports both HTTP and TLS
  listeners through the control socket.
- Gateway role without `--tls-listen` preserves the current status JSON shape as
  much as possible, with TLS represented as `null` if the field is added.
- DNS role behavior and status remain unchanged.

**Verification:**

- Completed with `--tls-listen` wired through `mvp-node gateway`, control
  socket status reporting `tls_listen_addr`, and DNS status remaining TLS-free.
- Verified with
  `cargo test --manifest-path MVP/Cargo.toml -p mvp-node --test product_serving_roles`.
- Verified with `cargo check --manifest-path MVP/Cargo.toml -p mvp-node`.

### U6B. Product ACME Issue Command

**Goal:** add a product-facing command that performs real ACME issuance and
publishes the certificate as durable MVP facts.

**Files:**

- `MVP/node/src/main.rs`
- `MVP/node/src/acme.rs` or a focused module under `MVP/node/src/`
- `MVP/node/src/error.rs`
- `MVP/node/src/state.rs`
- `MVP/node/Cargo.toml`
- `MVP/acme-command/src/p2panda.rs` if the certificate activation writer belongs
  beside existing ACME command adapters.
- `MVP/acme-command/src/tests.rs`
- `MVP/node/tests/product_acme.rs`

**Approach:**

- Add `mvp-node acme-issue --state <dir> --hostname <host> --gateway <url>
  [--issuer-holder <id>] [--account-path <path>]`.
- Load `AcmeIssuerConfig::from_env()` so Pebble uses
  `PLOYZ_ACME_DIRECTORY_URL` and `PLOYZ_ACME_ROOT_CA_PATH`.
- Persist account credentials under the node state directory unless
  `--account-path` is provided. The account store should key records by
  directory URL and serialize first-use creation locally.
- Use `InstantAcmeIssuer` with a publisher backed by `PandaAcmeHttp01Publisher`.
- Implement `AcmeHttp01ChallengeReadiness` by polling the supplied gateway URL
  for each challenge token before `set_ready`.
- After finalization, write `AcmeCertificateActivatedFact` through the canonical
  p2panda fact store with the same authority/manual-admission rules used by
  other product fact writers.
- Return structured command output that includes hostname, order URL, issued-at,
  not-before/not-after when present, and visible nodes at decision time.

**Test Scenarios:**

- Account credentials are created once and reused on the second issue attempt
  for the same directory URL.
- Malformed account credentials fail with a structured node error and do not
  write certificate activation facts.
- Challenge readiness timeout fails visibly and leaves no certificate activation
  fact.
- Certificate activation fact is written with the issued certificate material
  and can be projected into the serving snapshot.
- Token-scoped clear still leaves a newer in-flight token untouched.

**Verification:**

- Completed with `mvp-node acme-issue`, node-local account persistence,
  p2panda HTTP-01 publication/clear, gateway readiness polling, certificate
  activation fact writing, and projection/snapshot refresh.
- Verified with `cargo test --manifest-path MVP/Cargo.toml -p mvp-acme`.
- Verified with `cargo test --manifest-path MVP/Cargo.toml -p mvp-acme-command`.
- Verified with
  `cargo test --manifest-path MVP/Cargo.toml -p mvp-node issue_writes_certificate_activation_into_serving_snapshot`.
- Verified with `cargo test --manifest-path MVP/Cargo.toml -p mvp-projection`.
- Verified with `cargo test --manifest-path MVP/Cargo.toml -p mvp-serving`.

### U6C. Pebble HTTPS E2E

**Goal:** prove the full product path against Pebble and the Pingora TLS
gateway, not a fake issuer.

**Files:**

- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/pebble_acme_https_contract.rs`
- `MVP/e2e/Cargo.toml`
- `MVP/scripts/pebble-acme-https-smoke.sh` if shell orchestration is simpler
  for Docker/Pebble setup
- `packaging/e2e/pebble/pebble-config.json` as reference only unless an MVP-local
  copy is needed

**Approach:**

- Reuse the non-MVP runner mechanics for Pebble and challtestsrv: start both
  containers, mount `packaging/e2e/pebble`, and use Pebble's root CA for client
  verification.
- Start a product node with gateway HTTP and TLS listeners.
- Deploy or otherwise publish a route for the ACME hostname to a trivial HTTP
  backend.
- Run `mvp-node acme-issue` against the HTTP gateway URL so HTTP-01 visibility
  is proven through the product gateway before `set_ready`.
- Project/reload serving snapshots after the certificate activation fact is
  written.
- Verify HTTPS using Pebble's root CA and SNI/Host matching the issued hostname.

**Test Scenarios:**

- Happy path issues a real Pebble certificate and `curl --cacert` against the
  Pingora TLS listener returns the backend body.
- The scenario fails before `set_ready` if the HTTP-01 token is not visible
  through the gateway.
- The scenario fails if the gateway has no TLS listener or does not serve the
  projected certificate.
- A second issuance run reuses the account record and rotates the active
  certificate snapshot revision.

**Verification:**

- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- pebble-acme-https-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-acme-http01-contract`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-serving`

### U6D. Final Gate

**Goal:** make the whole ACME/TLS slice reviewable and complete.

**Verification:**

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-acme`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-acme-command`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-projection`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-serving`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-acme-http01-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- pebble-acme-https-contract`

**Review Scope:**

- Run a lightweight self-review for U6A if it stays parser/wiring only.
- Run a real code review pass before committing U6B/U6C because they touch
  durable account state, p2panda fact writes, ACME protocol flow, and E2E
  orchestration.

## Test Plan

Run after implementation units land:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-acme`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-acme-command`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-projection`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-serving`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-acme-http01-contract`
- Pebble-backed MVP ACME/HTTPS smoke once added.

## Status

- 2026-05-20: Planned from current MVP state and the existing non-MVP
  Pebble/`instant-acme` implementation. Implementation not started.
- 2026-05-20: Unit 1 started. `mvp-acme` now exposes the issuer boundary,
  account/order/certificate domain types, account coordination shape, disabled
  issuer fallback, contact URI handling, and order URL origin validation.
  Verified with `cargo test --manifest-path MVP/Cargo.toml -p mvp-acme`.
- 2026-05-20: Unit 2 started. `mvp-acme` now includes an `InstantAcmeIssuer`
  behind the issuer trait. It creates/resumes real ACME accounts and orders via
  `instant-acme`, publishes HTTP-01 challenges through the MVP publisher trait,
  waits on a readiness trait before `set_ready`, validates resumed order URL
  origins, finalizes to PEM material, and clears only the tokens owned by the
  order. Verified with `cargo test --manifest-path MVP/Cargo.toml -p mvp-acme`.
- 2026-05-20: Unit 3 started. `mvp-acme-command` now exposes
  `PandaAcmeHttp01Publisher`, which maps issuer challenges onto existing
  p2panda claim/present facts and clears by reconstructing the active lease for
  the exact hostname/token. Verified with
  `cargo test --manifest-path MVP/Cargo.toml -p mvp-acme -p mvp-acme-command`.
- 2026-05-20: Unit 4 started. Projected serving state now includes active
  certificates as a separate concept from routes and HTTP-01 challenges.
  Certificate activation facts reduce to latest-per-host serving certificate
  projections, snapshots include certificate material, SQLite persists it, and
  `mvp-serving` can look up certs by canonical hostname. Verified with
  `cargo test --manifest-path MVP/Cargo.toml -p mvp-acme -p mvp-acme-command -p mvp-projection -p mvp-serving -p mvp-volume`.
- 2026-05-20: Unit 5 started. The Pingora gateway can now run an optional TLS
  listener alongside the existing HTTP listener, select projected certificate
  material by SNI, terminate TLS, and route the decrypted request through the
  same Pingora request path. Verified with
  `cargo test --manifest-path MVP/Cargo.toml -p mvp-serving` and
  `cargo check --manifest-path MVP/Cargo.toml -p mvp-node`.
