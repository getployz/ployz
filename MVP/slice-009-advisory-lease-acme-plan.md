---
title: Slice 009 Advisory Lease Facts And ACME Canary Plan
status: active
created: 2026-05-17
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - crates/ployzd/src/daemon/cert_coordination.rs
  - crates/ployz-cert-backends/src/instant_acme_issuer.rs
---

# Slice 009 Advisory Lease Facts And ACME Canary Plan

## Problem Frame

ACME is the next product canary because it forces the MVP to express
"one issuer should act on this challenge at a time" without importing a NATS
server lock topology or pretending iroh-docs is consensus.

The corrected contract is:

- every node is equal,
- the operator's connected node is the command consistency boundary,
- a command writes durably to local docs and returns,
- replication is eventual,
- leases are advisory facts, not hard cluster locks,
- resource-level enforcement owns real exclusivity,
- surviving races become reducer-visible conflict candidates.

The proof target:

> ACME challenge ownership can be expressed as advisory lease facts with TTL,
> renewal, epoch fencing, RAII release, deterministic conflict reduction, and
> stale-holder rejection, while command results report visible nodes at decision
> time instead of waiting for quorum.

## Why This Is Next

The previous Slice 008 prototype proved the rough shape but carried the wrong
contract: witness acks, future pin-fact replacement, and lease uncertainty as a
special blocking mode. The docs now reject that direction. The next slice should
repair the primitive before deploy or ACME work depends on it.

This slice is also a semantic-leverage check against old certificate
coordination. The new code should let ACME business behavior say "acquire an
advisory challenge lease" and "publish with the current fencing epoch" instead
of owning bus locks, quorum, retry, and stale-owner fencing.

## Scope

Implement a self-contained MVP slice under `MVP/`:

- add a reusable advisory lease domain crate,
- model lease resources, holders, epochs, TTL, renewal, release, and fencing
  tokens with typed values,
- make lease guards mintable only by the lease book,
- add RAII release-on-drop semantics for local holders,
- reduce immutable lease facts into deterministic lease state,
- keep conflicting claims as candidates and project a deterministic winner by
  `(epoch desc, content_hash asc)`,
- annotate superseded losers in projection/status data,
- add an ACME HTTP-01 coordination canary that uses lease epochs before
  publishing or deleting challenge state,
- add an E2E `lease-acme-contract` scenario and include it in the time-budgeted
  `all`,
- report visible nodes at decision time in the ACME command result shape,
- record semantic-leverage evidence against the old ACME coordination baseline.

Out of scope:

- talking to Let's Encrypt or any real ACME directory,
- replacing `instant-acme` in existing crates,
- full iroh-docs persistence for lease facts,
- quorum, `min_replicas`, pin-fact commit paths, witness acks, or strict lease
  modes,
- wall-clock synchronization guarantees across real machines,
- gateway HTTP challenge serving,
- certificate issuance, account creation, or renewal scheduling,
- modifying existing `crates/` code.

## Crate Scout

Checked before planning this slice:

- `instant-acme` is the likely future ACME client layer because it is an async
  pure-Rust RFC 8555 client with first-class account/order/challenge types:
  <https://docs.rs/instant-acme/latest/instant_acme/>
- `rustls-acme` is valuable when ACME is folded into a rustls serving stream,
  but that is too coupled for this slice's ownership primitive:
  <https://docs.rs/rustls-acme/latest/rustls_acme/>
- `scopeguard` is a well-tested RAII helper, but the lease guard needs a typed
  domain release path and test-visible release facts. Use Rust `Drop` directly
  for the small MVP guard unless implementation proves a generic guard helper
  reduces code without hiding domain behavior:
  <https://docs.rs/scopeguard/latest/scopeguard/>

Decision: implement advisory leases directly as a small Ployz fact/reducer
model. Copy the proven ideas, not an external lock server.

## Key Decisions

### Advisory Lease Facts, Not Cluster Locks

Lease facts are command-level coordination evidence and fencing tokens. They do
not guarantee exclusivity across partitions. ACME, storage, and any future
resource adapter must still enforce its own conflict/fencing rules.

### Local Command Boundary

Lease acquisition reads the local fact view before mutation and writes a local
claim fact when it proceeds. The command result reports visible nodes at
decision time. It does not wait for peer acknowledgements.

### Deterministic Supersession

Conflicting same-resource claims remain candidates. Reducers order candidates by
`(epoch desc, content_hash asc)`. The projected winner may act locally; losers
are reported as superseded status.

### RAII Release Is Best Effort

A local guard drop should record a release fact through the owned lease book
when possible. Dropping must not panic and must not pretend release replicated.
Explicit release remains available for callers that need a foreground result.

## Implementation Units

### U1: Advisory Lease Domain Crate

Files:

- create `MVP/lease/Cargo.toml`
- create `MVP/lease/src/lib.rs`
- update `MVP/Cargo.toml`

Responsibilities:

- define typed lease identities:
  - `LeaseResource`
  - `LeaseHolder`
  - `LeaseEpoch`
  - `LeaseTimestamp`
  - `LeaseDuration`
  - `VisibleNode`
- define immutable lease facts:
  - `LeaseClaimed`
  - `LeaseRenewed`
  - `LeaseReleased`
- define `LeaseGuard`, `LeaseState`, `LeaseDecision`, `LeaseSuperseded`, and
  structured `LeaseError` variants.
- keep `LeaseGuard` construction private to the crate's owning book.
- implement deterministic reduction over candidate facts.
- implement explicit release and best-effort RAII drop release.

Test scenarios:

- first local claim becomes active and reports visible nodes,
- active claim returns a structured conflict before mutation,
- renewal by current holder extends expiry,
- renewal by stale holder fails,
- explicit release ends ownership,
- dropping a local guard records a release fact,
- expired lease allows a new holder with incremented epoch,
- conflicting same-epoch claims reduce deterministically and mark the loser
  superseded,
- zero epochs are unrepresentable,
- resource construction encodes delimiter-bearing segments.

Verification:

- `cd MVP && cargo test -p mvp-lease`

### U2: ACME Coordination Canary

Files:

- create `MVP/acme/Cargo.toml`
- create `MVP/acme/src/lib.rs`
- update `MVP/Cargo.toml`

Responsibilities:

- define ACME-facing identifiers:
  - `AcmeHostname`
  - `AcmeChallengeToken`
  - `AcmeKeyAuthorization`
- map hostname/token to encoded lease resources,
- expose a small `AcmeChallengeCoordinator` that:
  - acquires an advisory challenge lease,
  - publishes challenge state only with the current lease epoch,
  - refuses stale or superseded holders,
  - releases challenge ownership explicitly or through guard drop.
- keep this as a canary; no real ACME directory calls.

Test scenarios:

- first issuer acquires and publishes challenge state,
- second issuer sees structured conflict while first lease is active,
- after expiry, second issuer acquires a newer epoch,
- stale first issuer cannot publish or delete challenge state,
- deterministic supersession rejects a loser after conflicting claims converge,
- hostname normalization and token resource encoding are deterministic.

Verification:

- `cd MVP && cargo test -p mvp-acme`

### U3: E2E Lease/ACME Contract

Files:

- create `MVP/e2e/src/lease_acme_contract.rs`
- update `MVP/e2e/src/main.rs`
- update `MVP/e2e/Cargo.toml`

Responsibilities:

- add `lease-acme-contract` scenario,
- run the ACME canary with two issuers and exact assertions,
- emit JSON metrics for:
  - visible nodes at decision time,
  - contention/conflict detected,
  - stale mutation rejected,
  - expired lease takeover,
  - superseded candidate count,
  - elapsed milliseconds.
- include the scenario in `all`.

Test scenarios:

- `cargo run -p mvp-e2e -- lease-acme-contract`
- `MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all`

### U4: Maintainer Docs And Decision Ledger

Files:

- update `MVP/primitive-decisions.md`
- update `MVP/e2e-proof-plan.md`
- create `MVP/slice-009-advisory-lease-acme.md`

Responsibilities:

- record why the lease is advisory,
- record why witness/pin/quorum behavior is out of scope,
- record the crate scout,
- record semantic-leverage evidence against old ACME coordination.

Verification:

- docs contain no `store.pin_fact`, `min_replicas`, witness-ack, or strict
  lease language for this slice.

## Review Risks

- Accidentally reintroducing quorum language through metrics or tests.
- Making RAII drop perform fallible foreground work or hide failures.
- Letting ACME accept forged guards.
- Treating deterministic supersession as a real resource lock.
- Overbuilding real ACME protocol plumbing before gateway challenge serving is
  proven.

## Shipping Checks

Required before committing implementation:

```text
cd MVP && cargo fmt --all --check
cd MVP && cargo test -p mvp-lease -p mvp-acme
cd MVP && cargo run -p mvp-e2e -- lease-acme-contract
cd MVP && cargo clippy --all-targets -- -D warnings
```

Before pushing the slice:

```text
cd MVP && cargo test --all
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
just test
```
