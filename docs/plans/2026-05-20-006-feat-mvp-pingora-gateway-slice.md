---
title: MVP Data-Plane Parity Slice 4 Pingora Gateway
status: active
created: 2026-05-20
type: feature
parent_plan: docs/plans/2026-05-20-002-feat-mvp-data-plane-parity.md
---

# MVP Data-Plane Parity Slice 4 Pingora Gateway

## Problem Frame

The MVP gateway still uses a small hyper HTTP/1 server and rejects any backend
address that is not loopback. That was acceptable for process fixtures, but it
is now wrong for the Docker data plane: Docker runtime returns overlay
container endpoints, and every equal node gateway must proxy to backends on
other nodes through the overlay.

This slice replaces the gateway runtime with a Pingora-backed gateway while
keeping serving state, snapshot loading, and ACME HTTP-01 challenge lookup
behind existing `WireServingState` boundaries. The missing concept is a gateway
engine boundary: serving projection owns what should be served, while the
gateway engine owns HTTP/TLS/proxy execution.

## Current Evidence

- `MVP/serving/src/http_gateway.rs` currently owns listener accept, request
  parsing, ACME path handling, backend selection, backend I/O, response parsing,
  metrics, and shutdown.
- `parse_backend` rejects non-loopback addresses, which blocks overlay
  backends returned by Docker runtime.
- Pingora crate surface checked with `cargo info pingora --verbose`; current
  crate version is `0.8.0`, with `proxy`, `rustls`, and core HTTP crates
  available as features.

## Design Decisions

### Gateway Engine Is Below Serving State

`WireServingState` remains the source of route, DNS, status, and ACME challenge
answers. Pingora should call that boundary; it should not read projection
snapshots directly or mutate serving state.

### ACME HTTP-01 Stays Before Proxy Routing

HTTP-01 challenge paths must be served from ACME challenge projection before
normal route lookup. That preserves current behavior and is required for Pebble
issuance in Slice 5.

### Overlay Backends Are Valid Gateway Targets

The Pingora gateway must accept backend `SocketAddr`s from projection even when
they are Docker bridge or WireGuard overlay addresses. Loopback-only validation
must remain limited to process-fixture tests, not the production gateway path.

### Hyper Stays Only As A Test/Fixture Fallback If Needed

If keeping the current hyper path helps narrow tests, it should be renamed as a
fixture implementation. The node gateway command should use Pingora by default
for Linux data-plane work.

### TLS Hooks Land Before Real ACME Issuance

This slice should define certificate loading/reload shape, but real Pebble
issuance and certificate storage land in Slice 5. It is acceptable for this
slice to use static test certs or no TLS in focused HTTP tests, as long as the
gateway engine has the explicit TLS boundary.

## Implementation Units

### Unit 1: Gateway Engine Boundary

Files:

- `MVP/serving/src/gateway.rs`
- `MVP/serving/src/http_gateway.rs`
- `MVP/serving/src/lib.rs`

Work:

- Introduce a small gateway engine abstraction around start/shutdown/status.
- Keep `WireServingState` as the input boundary.
- Move shared host extraction, ACME path parsing, backend parsing, and response
  helpers behind engine-neutral functions where useful.

Tests:

- existing gateway unit tests still pass,
- new tests show overlay backend addresses are accepted by the production
  gateway path.

### Unit 2: Pingora HTTP Proxy Engine

Files:

- `MVP/serving/Cargo.toml`
- `MVP/serving/src/pingora_gateway.rs`

Work:

- Add Pingora dependencies with the smallest feature set needed for HTTP proxy
  and later TLS (`proxy`, likely `rustls`).
- Implement Pingora request handling over `WireServingState`.
- Preserve metrics semantics: request count, backend failures, latency samples.
- Preserve bounded backend connect/read/write behavior.

Tests:

- proxy to a non-loopback/overlay-style backend address in a controlled test,
- unknown host and missing backend responses match existing behavior,
- ACME HTTP-01 path is served before route proxying.

### Unit 3: Node Gateway Default And Compatibility

Files:

- `MVP/node/src/serving.rs`
- `MVP/node/src/main.rs`
- `MVP/e2e/src/wire_serving_contract.rs`

Work:

- Make `mvp-node gateway` start the Pingora engine by default.
- Keep control socket readiness/status/reload/shutdown behavior unchanged.
- Ensure stale/last-good serving semantics still come from `ServingActorHandle`.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- existing wire serving E2E still passes.

### Unit 4: TLS Hook And Static Cert Smoke

Files:

- `MVP/serving/src/pingora_gateway.rs`
- `MVP/serving/src/model.rs`
- `MVP/node/src/serving.rs`

Work:

- Define TLS certificate source/reload shape without implementing real ACME
  issuance yet.
- Add a static/test certificate path accepted by the Pingora engine for HTTPS
  smoke coverage.
- Keep HTTP-01 challenge serving available on HTTP.

Tests:

- static-cert HTTPS request reaches the gateway and validates with a supplied
  test root or explicit test client configuration,
- certificate reload failure preserves last-good gateway behavior.

## Acceptance Checklist

- `mvp-node gateway` uses Pingora by default.
- HTTP proxying works to overlay/non-loopback backend addresses.
- ACME HTTP-01 challenge paths remain compatible with existing projection
  facts.
- Gateway control role readiness/status/reload/shutdown remains compatible.
- Metrics still record requests, backend failures, and latency samples.
- TLS hook exists for Slice 5 Pebble-issued certificates.

## Verification Commands

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-serving`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- wire-serving-contract`

## Explicit Deferrals

- Pebble/`instant-acme` issuance remains Slice 5.
- Full three-node HTTPS parity smoke remains the final smoke slice.
- Pingora performance tuning is out of scope until functional parity passes.
