---
title: Slice 024 ACME Command Surface Plan
status: completed
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-021-p2panda-acme-http01-plan.md
  - MVP/slice-023-owned-p2panda-net-transport.md
external:
  - https://docs.rs/instant-acme/
  - https://docs.rs/crate/instant-acme/latest
  - https://docs.rs/rustls-acme
  - https://docs.rs/crate/rustls-acme/latest
---

# Slice 024 ACME Command Surface Plan

## Problem Frame

The MVP has proved ACME HTTP-01 three ways: in-memory advisory leases,
docs-backed facts, p2panda-sync, and owned p2panda-net transport. The product
semantics are now credible, but the useful ACME command adapter still lives in
`MVP/e2e/src/p2panda_acme_http01_contract.rs`. That means the best business
logic from the canary is not a reusable foundation primitive yet.

The next slice should be deletion-backed: extract the ACME command surface into
an MVP-local library, reuse it from the p2panda ACME E2Es, and delete duplicated
E2E-local command logic. This is the first honest semantic-leverage proof after
Slice 023: future product code should call typed ACME commands instead of
relearning lease replay, fencing, preflight authorization, visible-node results,
and fact writes.

## Requirements Trace

- `VISION.md`: operations are command-shaped, explicit, visible, and safe to
  retry. ACME challenge presentation/clear must be a foreground command result,
  not a background reconciler.
- `MVP/overall-plan.md`: the next slice should be deletion-backed and should
  choose a real vertical path, preferably ACME HTTP-01 first.
- `MVP/e2e-proof-plan.md`: ACME is the advisory lease-fenced singleton canary;
  current proof status still calls out the old cert coordination path as the
  LOC baseline.
- `MVP/primitive-decisions.md`: ACME leases are advisory fencing tokens; real
  exclusivity lives at the ACME directory/challenge validation layer.
- User direction: keep code simple, easy to maintain, easy to understand, and
  bias toward maintained crates when they reduce plumbing.

## Dependency Scout

Checked before planning:

- `instant-acme` is an async pure-Rust RFC 8555 ACME client. The latest docs.rs
  crate page describes it as used in production and built on Tokio/rustls. It
  is the best candidate when the MVP starts real ACME order/account/challenge
  protocol work.
- `rustls-acme` manages certificates around rustls serving and exposes lower
  certificate-management state/resolver pieces. It is useful when TLS serving
  integration becomes the slice target.
- `acme-client` exists, but the current MVP does not need another ACME protocol
  client until real issuance is in scope.

Decision:

- Add no ACME protocol dependency in this slice. The slice is about the Ployz
  command surface and fact semantics, not talking to Let's Encrypt/Pebble.
- Keep `instant-acme` noted as the likely future protocol client once issuing
  real certificates is the proof target.

## Scope

In scope:

- Add a reusable MVP-local ACME command crate or module that owns:
  - claim,
  - present HTTP-01,
  - clear HTTP-01,
  - local lease-state replay from a `FactSource`,
  - authorization preflight before mutation,
  - visible nodes at decision time,
  - structured command errors.
- Reuse existing `mvp-acme`, `mvp-lease`, `mvp-p2panda-facts`, and
  `mvp-projection` payload types. Do not duplicate fact schemas.
- Move the p2panda ACME E2E contracts onto the new command surface.
- Delete the E2E-local `AcmeP2pandaCommandAdapter`, error enum, lease replay,
  preflight, and duplicated stale/clear helpers once replaced.
- Preserve the existing `p2panda-acme-http01-contract` and
  `p2panda-net-acme-http01-contract` behavior.
- Add focused unit tests for command preconditions and fact writes.
- Record semantic leverage: LOC removed from E2E, LOC added to reusable command
  code, and the old-code baseline:
  - `crates/ployzd/src/daemon/cert_coordination.rs`: 520 LOC
  - `crates/ployz-cert-backends/src/*.rs`: 535 LOC
  - current `MVP/e2e/src/p2panda_acme_http01_contract.rs`: 1,653 LOC

Out of scope:

- Real ACME account/order/authorization/challenge protocol.
- Certificate material storage, renewal scheduling, or TLS installation.
- Pingora TLS integration.
- DNS-01.
- `PhasedCommand`; ACME has command preflight and fencing but not enough
  multi-phase resume/compensation weight to justify the generic primitive.
- Modifying existing non-MVP crates.

## Design Decisions

### Command Surface, Not ACME Client

The new code should be named around Ployz command semantics, not around the
ACME protocol client. It should express:

```text
claim challenge ownership
present HTTP-01 challenge
clear HTTP-01 challenge
```

Real issuance can later call these commands around `instant-acme` activities.
This slice should not make the command surface depend on an ACME directory.

### FactSource For Reads, Fact Writer For Mutations

Lease preconditions should read through a `FactSource`-shaped boundary and write
through a narrow fact writer. This avoids coupling command logic to p2panda
store internals while keeping p2panda-backed E2Es realistic.

The command must fail closed if relevant lease candidates are unreadable,
malformed, unauthorized, or mismatched with their key.

### Preflight Before Mutation

`clear` must prove both the lease-release fact and ACME-clear fact are writable
before appending either. This avoids partial release if ACME clear authorization
fails.

### Delete E2E Business Logic

The E2E should keep wiring, transport, projection, and serving proof. It should
not own the ACME command state machine. After this slice, the ACME E2E should
read as product behavior over the command crate.

## Implementation Units

### Unit 1: Command Crate Boundary

Files:

- `MVP/Cargo.toml`
- `MVP/acme-command/Cargo.toml`
- `MVP/acme-command/src/lib.rs`
- `MVP/acme-command/src/error.rs`
- `MVP/acme-command/src/facts.rs`
- `MVP/acme-command/src/tests.rs`

Work:

- Add `mvp-acme-command`.
- Define typed command inputs/results:
  - `AcmeClaimCommand`,
  - `AcmePresentHttp01Command`,
  - `AcmeClearHttp01Command`,
  - `AcmeLeaseHandle`,
  - result types carrying visible nodes.
- Define narrow traits or concrete helpers for reading candidates and writing
  projection payload facts without importing E2E helpers.
- Keep errors structured: conflict, stale lease, challenge mismatch, malformed
  candidate, unreadable candidate, unauthorized fact, write failure.

Tests:

- Active lease conflict fails before mutation.
- Expired/released lease can be claimed at a higher epoch.
- Present requires the current holder, epoch, and claim hash.
- Clear preflights both release and clear fact keys before writing.
- Malformed key/payload lease candidates fail closed.
- Command results include visible nodes.

### Unit 2: P2panda Fact Writer Adapter

Files:

- `MVP/acme-command/src/p2panda.rs`
- `MVP/acme-command/src/tests.rs`
- `MVP/p2panda-facts/src/lib.rs` only if a tiny public helper is needed

Work:

- Add a small adapter that writes `ProjectionFactPayload` through
  `PandaFactStore` and `PandaFactAuthor`.
- Trust and author-key binding remain owned by `PandaFactStore`; the command
  crate should not bypass `can_write_fact`.
- Avoid an ACME-specific sync/import path.

Tests:

- P2panda-backed command writes lease and ACME facts with the same keys used by
  existing projections.
- Principal/session mismatch fails before writing.
- Scoped grants prevent unrelated challenge writes.

### Unit 3: Replace E2E-Local ACME Adapter

Files:

- `MVP/e2e/Cargo.toml`
- `MVP/e2e/src/p2panda_acme_http01_contract.rs`

Work:

- Replace `AcmeP2pandaCommandAdapter`, local `AcmeAdapterError`,
  `lease_state_from_store`, `assert_current_lease`, `preflight_fact_write`, and
  stale helper code with `mvp-acme-command`.
- Keep the p2panda-sync and p2panda-net scenario flows intact.
- Keep the E2E responsible for transport, projection, serving reload, and
  metrics.

Tests:

- `p2panda-acme-http01-contract` still proves:
  - conflict-at-entry,
  - scoped challenge grant rejection,
  - stale present rejection before mutation,
  - release fact recorded,
  - HTTP-01 serving while issuer adapter is absent,
  - clear to 404,
  - SQLite rebuild after delete.
- `p2panda-net-acme-http01-contract` still proves the same command path over
  owned p2panda-net transport.

### Unit 4: Simplification And Semantic-Leverage Report

Files:

- `MVP/slice-024-acme-command-surface.md`
- `MVP/e2e-proof-plan.md`
- `MVP/overall-plan.md`
- `MVP/primitive-decisions.md`

Work:

- Record what was deleted from E2E and what moved into reusable command code.
- Compare ACME command surface LOC against old cert coordination baseline.
- Note that `instant-acme` remains the future protocol-client candidate, not a
  dependency in this slice.
- Update proof status for E2E-6a and E2E-9 semantic leverage.

## Verification

Targeted gates:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-acme-command
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-acme-http01-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-acme-http01-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- docs-backed-acme-http01-contract
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-acme-command -p mvp-e2e --all-targets -- -D warnings
```

Full gate before pushing the closeout:

```bash
MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
```

## Review Risks

- Accidentally reintroducing partial mutation in `clear`.
- Letting unreadable or malformed lease candidates be ignored as vacant state.
- Making ACME command code depend on p2panda transport types.
- Hiding business rules behind generic workflow abstractions too early.
- Leaving the E2E with duplicated command logic after adding the command crate.
- Treating real ACME issuance as in scope and pulling in protocol dependencies
  before the command surface is clean.
