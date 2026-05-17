---
title: Slice 015 Docs-Backed ACME HTTP-01 Plan
status: active
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-009-advisory-lease-acme-plan.md
  - MVP/slice-011-steady-state-serving-plan.md
  - MVP/slice-013-wire-http-dns-serving-plan.md
  - MVP/slice-014-membership-wireguard-plan.md
---

# Slice 015 Docs-Backed ACME HTTP-01 Plan

## Problem Frame

ACME is the next product canary because it forces the new primitives to answer
a concrete question:

> How does one issuer publish a challenge response without pretending the
> cluster has a linearizable lock?

Slice 009 proved advisory lease semantics in memory. This slice moves that
proof onto docs-backed facts and wires the result into HTTP serving:

```text
connected node writes advisory lease/challenge facts locally
  -> facts replicate through iroh-docs
  -> projection reduces lease/challenge candidates
  -> gateway snapshot contains HTTP-01 challenge responses
  -> HTTP gateway serves /.well-known/acme-challenge/<token>
```

There is no quorum, no witness acknowledgement, no `store.pin_fact`, and no
hidden active-partition gate. The operator's connected node remains the command
consistency boundary. Future memberlist or active-partition evidence may enrich
preflight checks later, but it is explicitly not part of this commit boundary.

## Requirements Traceability

- `VISION.md`: the daemon/coordinator is disposable; the data plane must keep
  working from last-applied state when the control plane is down.
- `MVP/overall-plan.md`: ACME should be the next product proof, and leases are
  advisory foreground coordination hints, not cluster locks.
- `MVP/architecture.md`: facts commit durably on the connected node and
  replicate eventually; surviving races remain reducer candidates and command
  results include visible nodes at decision time.
- `MVP/e2e-proof-plan.md`: E2E-6a still needs docs-backed lease facts over real
  iroh replication and gateway HTTP challenge serving.
- `MVP/primitive-decisions.md`: resource-level enforcement owns real
  exclusivity. For ACME, that is the ACME directory plus challenge validation;
  the Ployz lease only gates command entry and carries a fencing token.
- `MVP/slice-009-advisory-lease-acme-plan.md`: keep TTL, renewal, epoch
  fencing, RAII release-on-drop for local holders, stale mutation rejection,
  deterministic supersession, and local-only command success.
- `MVP/slice-011-steady-state-serving-plan.md` and
  `MVP/slice-013-wire-http-dns-serving-plan.md`: serving roles preserve last
  good snapshots, surface freshness/failure, and must not call back into the
  coordinator to answer data-plane traffic.
- `MVP/slice-014-membership-wireguard-plan.md`: visible nodes are explicit
  decision evidence, not a peer-ack commit rule.

## Scope

Implement a self-contained ACME HTTP-01 proof under `MVP/`:

- docs-backed advisory lease facts for ACME challenge ownership,
- docs-backed ACME HTTP-01 challenge presentation and clear facts,
- deterministic reduction of lease/challenge conflict candidates,
- gateway snapshot support for HTTP-01 challenge responses,
- HTTP gateway serving for `/.well-known/acme-challenge/<token>`,
- an E2E scenario named `docs-backed-acme-http01-contract`,
- metrics for acquisition, docs propagation, projection, gateway reload,
  one HTTP challenge request duration, contention, stale rejection, and
  supersession,
- maintainer documentation updates in `MVP/primitive-decisions.md`,
  `MVP/e2e-proof-plan.md`, and the slice result report after implementation.

Out of scope:

- real certificate issuance against Let's Encrypt or another ACME directory,
- TLS certificate installation or renewal scheduling,
- DNS-01,
- account key generation/storage beyond key-authorization validation rules,
- ACME order/account/authorization lifecycle,
- a certificate-manager facade,
- Pingora integration,
- production process supervision,
- strict lease mode, quorum mode, witness acknowledgements, or `store.pin_fact`,
- automatic active-member or partition-view checks,
- changing code outside `MVP/`.

The product behavior being proven is challenge ownership and serving, not a
complete certificate manager.

## Crate Scout

Checked before planning:

- `instant-acme` 0.8.5 is an async pure-Rust ACME RFC 8555 client with typed
  account, order, authorization, challenge, and key-authorization concepts:
  <https://docs.rs/instant-acme/latest/instant_acme/>. It is the best candidate
  when this MVP starts talking to a real ACME directory, but this slice does
  not need network CA behavior.
- `rustls-acme` 0.15.2 offers a stream-oriented rustls ACME integration with
  TLS-ALPN-01 and HTTP-01 support:
  <https://docs.rs/rustls-acme/latest/rustls_acme/>. It is too coupled to TLS
  serving and certificate acquisition for this slice's primitive proof.
- `hyper` 1.9 is already used in `MVP/serving`; its docs describe it as a
  low-level, fast, correct HTTP building block with server APIs behind the
  `server` feature: <https://docs.rs/hyper/latest/hyper/>. Keep using the
  existing Hyper gateway proof instead of adding Axum or another framework.
- RFC 8555 section 8.3 defines HTTP-01: provision the key authorization at
  `/.well-known/acme-challenge/<token>` and serve it over HTTP:
  <https://datatracker.ietf.org/doc/html/rfc8555#section-8.3>.

Decision for this slice: add no ACME client dependency. Keep ACME protocol
shape as typed domain data and use the existing Hyper serving path. Revisit
`instant-acme` when the next slice needs real CA order/challenge polling.

## Design Decisions

### Challenge Serving Is Projected Data-Plane State

The HTTP gateway must not ask the coordinator whether a challenge exists on
each request. ACME challenge responses are reduced from facts into the gateway
snapshot and loaded into last-good serving state. If the coordinator is killed,
the gateway keeps serving the last good response.

### Advisory Lease TTL Gates Mutation, Not Serving Liveness

A lease decides whether an issuer may publish or clear a challenge fact at
command time. Once a valid challenge presentation fact exists, serving is a
data-plane consequence of that fact until a clear fact or a higher-epoch
presentation supersedes it. The gateway should not silently remove a challenge
because the coordinator process crashed or because a local guard was dropped.

### Same-Key Conflicts Must Reach Domain Reducers

Current docs-backed fact sources can surface same-key conflict candidates.
This slice must make those candidates reducer-visible for supported fact kinds
instead of treating every `CandidateStatus::Conflict` as an immediate
projection ignore. Unauthorized, unverified, cross-island, missing-payload, and
malformed candidates still remain rejected/ignored before domain logic.

The domain reducer then orders conflict candidates deterministically by:

```text
(epoch desc, content_hash asc)
```

Losers become superseded projection status. The operator does not pick a
winner.

Use explicit enum dispatch for lease and ACME fact kinds only. Do not introduce
a reducer plugin system, generic domain registry, workflow framework, or new
abstraction layer in this slice.

### Authority And Fact Grants

ACME issuers need explicit fact authority. Docs access alone is not enough.

Grant shape for the canary:

```text
issuer:
  write /facts/lease/<resource>/>
  write /facts/acme/http01/<hostname>/<token>/>
projection:
  read /facts/lease/>
  read /facts/acme/http01/>
```

The implementation should keep lease fact writes and challenge fact writes as
separate grants. A principal allowed to claim a lease is not automatically
allowed to present or clear an HTTP-01 challenge for a host unless the island
grant says so. Unauthorized writers must fail before docs mutation where the
writer is local, and imported unauthorized facts must remain unreadable
candidates that cannot affect projection.

### Challenge Identity And Host Canonicalization

`AcmeChallengeId` should be the only constructor/parser for:

- canonical hostname,
- token,
- lease resource,
- lease fact keys,
- ACME challenge fact keys,
- projection lookup key.

Hostname policy for this slice:

- ASCII DNS names only; IDNA conversion is out of scope, so callers must pass
  punycode already encoded when needed,
- lower-case,
- trim one or more trailing dots,
- reject empty hosts, empty labels, labels over 63 bytes, hosts over 253 bytes,
  non-ASCII labels, wildcard labels, and characters outside `[a-z0-9-]`,
- reject labels that start or end with `-`,
- strip a `Host` header port consistently before canonicalization,
- require the fact-key hostname and payload hostname to canonicalize to the
  same value.

This avoids drift between lease resources, ACME fact keys, projection rows,
snapshots, and HTTP lookup.

### Fact Keys

Use fact keys that make command races visible as same-key candidates:

```text
/facts/lease/<resource>/claimed/<epoch>
/facts/lease/<resource>/renewed/<epoch>/<claim_hash>/<renewed_at>
/facts/lease/<resource>/released/<epoch>/<claim_hash>/<released_at_or_drop>
/facts/acme/http01/<hostname>/<token>/presented/<epoch>
/facts/acme/http01/<hostname>/<token>/cleared/<epoch>/<claim_hash>
```

`<resource>`, `<hostname>`, and `<token>` must come from `AcmeChallengeId`.
`<resource>` is the existing encoded lease resource string, such as
`acme.http01.example.com.token`. The path uses a single segment for the encoded
resource so wildcard grants can stay simple.

### Fact Payloads

Extend the projection/fact payload model with typed data for:

- `LeaseClaimed`,
- `LeaseRenewed`,
- `LeaseReleased`,
- `AcmeHttp01Presented`,
- `AcmeHttp01Cleared`.

The ACME presentation fact carries hostname, token, key authorization, holder,
lease epoch, claim hash, and published-at timestamp. Clear carries hostname,
token, holder, lease epoch, claim hash, and cleared-at timestamp.

Do not store raw account private keys or certificate material in these facts.

`AcmeKeyAuthorization` validation is part of this slice:

- ASCII only,
- no whitespace or control characters,
- bounded length,
- exact `<token>.<thumbprint>` shape,
- token part equals the challenge token from the fact key and payload,
- token and thumbprint use base64url characters without padding,
- a bare token echo is invalid.

### HTTP Semantics

For `GET /.well-known/acme-challenge/<token>`:

- strip any `Host` header port and canonicalize through the ACME host newtype,
- validate token syntax before lookup,
- look up `(hostname, token)` in last-good serving state,
- return `200` with the ASCII key authorization and no trailing newline when
  present,
- return `404` when absent,
- preserve existing `405` behavior for non-GET requests.

Challenge lookup happens before normal route proxying so application backends
do not need special ACME routes.

Serving last-good challenge state is intentional, but it must be visible. The
projection/snapshot/serving status should expose active challenge count and
published-at age so an abandoned or uncleared challenge is not hidden behind a
healthy HTTP response.

## Implementation Units

### Unit 1: Serializable Lease And ACME Fact Model

Files:

- `MVP/lease/src/lib.rs`
- `MVP/acme/src/lib.rs`
- `MVP/projection/src/actor.rs`
- `MVP/projection/src/facts.rs`
- `MVP/projection/src/source.rs`
- `MVP/projection/src/reducer.rs`

Work:

- Add serde support to lease fact structs without weakening existing newtypes.
- Add ACME presentation/clear fact structs and conversions from existing
  `AcmeChallengeLease` and `AcmeKeyAuthorization`.
- Extend `FactKind`, `classify_fact_key`, `ProjectionFactPayload`, and
  key/payload validation for lease and ACME keys.
- Change reducer conflict handling so supported authorized conflict candidates
  with readable payloads are reduced by domain logic.
- Change projection actor payload prefetch so authorized supported conflict
  candidates can be read by reducers, while unauthorized, unverified, and
  cross-island candidates remain unread.
- Keep the conflict-handling change explicit to lease and ACME fact kinds for
  this slice.

Tests:

- `MVP/lease/src/lib.rs`: lease facts serialize/deserialize without losing
  resource, holder, epoch, timestamps, or claim hash.
- `MVP/acme/src/lib.rs`: presented/cleared facts require matching token and
  current lease epoch/claim hash.
- `MVP/acme/src/lib.rs`: `AcmeChallengeId` is the only path from hostname and
  token to lease resource and fact keys.
- `MVP/acme/src/lib.rs`: key authorization rejects mismatched token, bare token
  echo, newline/CRLF, non-ASCII, invalid base64url characters, padding, and
  overlong values.
- `MVP/acme/src/lib.rs`: host canonicalization handles mixed case, trailing
  dot, port-bearing `Host`, invalid labels, and key/payload hostname mismatch.
- `MVP/projection/src/source.rs`: lease and ACME fact keys classify with the
  correct kind and epoch.
- `MVP/projection/src/reducer.rs`: same-key authorized conflict candidates are
  visible to the reducer and unauthorized conflict candidates remain unread.

### Unit 2: ACME Projection And Gateway Snapshot

Files:

- `MVP/projection/src/model.rs`
- `MVP/projection/src/reducer.rs`
- `MVP/projection/src/sqlite.rs`
- `MVP/projection/src/snapshot.rs`
- `MVP/serving/src/model.rs`

Work:

- Add `AcmeHttp01ChallengeProjection` keyed by hostname and token.
- Store projected challenges in SQLite as disposable cache rows.
- Include projected challenges in `GatewaySnapshotFile`.
- Load and index challenges in `ServingSnapshotBatch`.
- Expose active challenge count and oldest/newest published-at values through
  snapshot/serving status or the narrow status shape used by the E2E role.
- Record superseded challenge/lease candidates in projection status.

Tests:

- Projection chooses the highest epoch challenge, then lowest content hash on a
  same-epoch race.
- A clear fact from the current epoch/claim removes the challenge.
- A stale clear fact cannot remove a newer presentation.
- SQLite rebuild round-trips projected challenges.
- Gateway snapshot load rejects wrong-island or malformed challenge data with
  the existing structured snapshot failures.
- Projection status reports superseded challenge candidates without requiring
  an operator-picked conflict resolution path.

### Unit 3: Backend-Neutral Command Model And Docs Adapter

Files:

- `MVP/acme/src/lib.rs`
- `MVP/iroh/src/facts.rs`
- `MVP/e2e/src/bus_syntax.rs`

Work:

- Keep `mvp-acme` backend-neutral. It may emit/consume typed fact DTOs or a
  narrow command trait, but it must not depend on `mvp-iroh`.
- Add a thin docs-backed adapter in `mvp-iroh` or the E2E harness that reads
  relevant local candidates before mutation, writes to the connected
  `IrohFactDoc`, and returns visible nodes at decision time.
- Enforce publish/clear by lease epoch and claim hash immediately before
  writing challenge facts.
- Enforce explicit issuer grants for lease and challenge facts.
- Preserve local-only success when the visible-node set is empty.
- Keep the helper thin: only acquire/present/clear operations needed by the
  acceptance gate. Status should come from existing lease/projection status
  surfaces, not from a new certificate-manager facade.

Tests:

- Second issuer receives a structured conflict before presentation while the
  first lease is locally visible and active.
- Expired lease allows a higher-epoch holder to present.
- Stale holder publish and clear are rejected and do not remove the current
  challenge.
- Local-only acquire/present succeeds and reports zero visible nodes.
- Unauthorized issuer cannot write lease facts or ACME challenge facts.
- Unauthorized imported challenge facts cannot affect projection or clear
  another holder's challenge.

### Unit 4: HTTP Gateway Challenge Serving

Files:

- `MVP/serving/src/actor.rs`
- `MVP/serving/src/wire.rs`
- `MVP/serving/src/http_gateway.rs`
- `MVP/serving/src/tests.rs`

Work:

- Add an ACME challenge lookup method to `ServingActorHandle` and
  `WireServingState`, backed by the actor-owned last-good snapshot.
- Serve ACME challenge paths before proxy route lookup.
- Use the projected key authorization body exactly as stored.
- Surface active/stale challenge visibility through the existing status path if
  that can stay narrow. Do not add request hit/miss counters or percentile
  aggregation in this slice.

Tests:

- `GET /.well-known/acme-challenge/<token>` returns `200` and the expected body
  for a loaded challenge.
- Unknown token and wrong host return `404`.
- Non-GET challenge request returns `405`.
- Existing route proxy behavior is unchanged for non-ACME paths.

### Unit 5: E2E Contract

Files:

- `MVP/e2e/src/docs_backed_acme_http01_contract.rs`
- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/metrics.rs`

Work:

- Add `docs-backed-acme-http01-contract`.
- Use two `IrohFactNode`s and a shared docs ticket.
- Write lease and presentation facts through node A's connected docs store.
- Wait for node B's docs local view to observe the facts through
  `IrohDocsFactSource`.
- Rebuild projection, write gateway snapshot, reload a real Hyper gateway, and
  assert HTTP-01 response on node B.
- Kill/drop the local command adapter after projection and verify the gateway
  still serves last-good challenge state.
- Write a higher-epoch presentation, project/reload, and verify the gateway
  serves the new response.
- Assert stale challenge age/status is visible after coordinator drop.

Keep stale clear/present rejection and same-epoch supersession primarily in
`mvp-acme`/`mvp-projection` tests unless a specific behavior requires
cross-node docs replication.

Metrics:

- visible nodes at decision time,
- acquire duration,
- docs propagation duration,
- projection duration,
- gateway reload duration,
- one HTTP challenge request duration,
- contention/conflict count,
- stale mutation rejection count,
- superseded candidate count,
- coordinator-dropped serving success count,
- stale challenge age/status,
- elapsed scenario time.

## Verification

Implementation should run:

```text
cd MVP && cargo fmt --all
cd MVP && cargo test -p mvp-lease -p mvp-acme -p mvp-projection -p mvp-serving -p mvp-iroh -p mvp-e2e
cd MVP && cargo clippy -p mvp-lease -p mvp-acme -p mvp-projection -p mvp-serving -p mvp-iroh -p mvp-e2e --tests -- -D warnings
cd MVP && cargo run -p mvp-e2e -- docs-backed-acme-http01-contract
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

If `mvp-e2e -- all` exceeds the current 120-second budget, the slice should
diagnose the added cost instead of raising the budget by default.

## Review Focus

Run review subagents before the implementation commit:

- correctness: lease epoch/claim-hash fencing, stale clear behavior, conflict
  candidate reduction,
- security: no account keys or certificate material in facts, no token echoing
  from request to response, no challenge path traversal,
- reliability: serving keeps last good state and ACME requests do not depend
  on the coordinator,
- simplicity: feature code should read as lease/check/present/project/serve,
  not transport or retry choreography.

Run `ce-simplify-code` after the first green E2E and commit that pass
separately.

## Semantic-Leverage Baseline

Old ACME reference code:

```text
crates/ployzd/src/daemon/cert_coordination.rs: 520 LOC
crates/ployz-cert-backends/src/*.rs: 535 LOC
```

The MVP slice should not aim for LOC reduction by omitting behavior. The
leverage target is a clearer shape:

- ACME ownership lives in lease/challenge facts and typed command outcomes.
- HTTP serving reads a projection, not a coordinator callback.
- Tests prove product behavior through docs replication and real HTTP serving.
- Future real issuance can plug in an ACME client without changing the
  ownership or serving primitives.

## Acceptance Gate

The slice is complete when:

- ACME lease and challenge facts are written through `IrohFactDoc`,
- a remote docs-backed projection produces a gateway snapshot with HTTP-01
  challenge state,
- Hyper gateway serves the challenge body from last-good projected state,
- stale publish/clear attempts are rejected with structured errors,
- same-epoch race candidates are reduced deterministically and surfaced as
  superseded,
- command results include visible nodes at decision time,
- no quorum, witness ack, pin-fact path, strict lease mode, or hidden
  active-partition gate is introduced,
- `mvp-e2e -- all` remains time-budgeted and green.
