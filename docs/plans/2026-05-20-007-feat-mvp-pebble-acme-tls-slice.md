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

Missing:

- MVP has no ACME issuer abstraction.
- MVP has no ACME account credential persistence.
- MVP has no order lifecycle or issued certificate model.
- MVP has no projected certificate material for the serving snapshot.
- Pingora is currently the default HTTP engine, but it does not yet serve TLS.
- No MVP Pebble-backed E2E validates a cert against Pebble's root.

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
