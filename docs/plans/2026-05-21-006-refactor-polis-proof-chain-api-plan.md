---
title: "refactor: Make Polis Enable Rusty Ployz Proof Chains"
type: refactor
status: active
date: 2026-05-21
origin: chat
supersedes:
  - docs/plans/2026-05-21-005-refactor-polis-capability-values-plan.md
depends_on:
  - docs/brainstorms/2026-05-21-polis-ployz-boundary-requirements.md
  - docs/plans/2026-05-21-003-refactor-polis-ployz-root-api-boundary-plan.md
  - docs/plans/2026-05-21-004-feat-domain-add-https-readiness-plan.md
---

# refactor: Make Polis Enable Rusty Ployz Proof Chains

## Summary

Redesign the Polis/Ployz seam from the desired Ployz application code
backward. Polis should not be a bucket of distributed-system modules, and Ployz
should not call framework bookkeeping APIs from product services. The target is
Ployz code that reads as a short chain of typed proofs:

```rust
runner.run(command, |ctx, request| {
    let claim = ctx.claim(DomainResource::for_domain(&request.domain))?;
    let cert = certificates.ensure_usable(ctx, &claim, &request.domain)?;
    let ready = serving.activate(ctx, &claim, cert)?;
    records.record_ready(ctx, ready.clone())?;
    Ok(ready)
})
```

Each value in that chain should mean something:

- `ctx` proves the command is authorized, open, and idempotent for this run.
- `claim` proves this command currently owns the guarded resource.
- `cert` proves certificate usability for the exact hostname.
- `ready` proves serving has activated the certificate for the domain.

Polis provides the generic capability machinery behind those values. Ployz owns
the product types, product ports, product failure mapping, and command
semantics.

This plan replaces the previous capability-values plan because the earlier
shape was still too tolerant of wrappers over the old API. The new completion
bar is code shape: Ployz feature code must become easier to read because Polis
is carrying the right complexity.

## Problem Frame

The domain HTTPS readiness spike proved useful behavior but failed the design
test. It introduced names that looked Rusty while leaving the hard sequencing
work in product code.

Current problems to correct:

- `crates/ployz/src/operation.rs` contains `OperationRunner`, but it wraps
  `record_evidence` and `terminalize` rather than owning an open operation
  value that closes by consuming transition.
- `crates/ployz/src/domain/mod.rs` has typestate-like transitions, but they
  mostly prove local call order. They do not prove durable replay, fresh claim
  ownership, certificate activation validity, or command idempotency.
- `DomainStatusPort::status` exists but current readiness flow does not use it
  to make ready reuse or regression detection explicit.
- Domain retry coverage is not real if the test calls `ensure_ready` once and
  only checks pending writes.
- Ployz feature modules can still end up understanding framework mechanics:
  operation IDs, idempotency, claims, evidence, terminal markers, and deadline
  plumbing.

The fix is not another crate split. The fix is to make the public API force a
better shape.

## Legacy MVP Grounding

Polis must be extracted from working pressure in `legacy/mvp`, not invented
from a clean-room API sketch. The target API should earn its shape by making
these existing cases simpler without erasing their constraints:

- `legacy/mvp/lease/src/lib.rs` already models the important lease mechanics:
  resource identity, holder, epoch, claim hash, expiry, renewal, release, and
  drop release. Polis claim APIs should preserve those realities.
- `legacy/mvp/acme/src/lib.rs` already shows the product wrapper pattern that
  should survive: `AcmeChallengeLease` validates that a generic lease guard
  matches an ACME challenge resource before challenge facts can be built.
- `legacy/mvp/volume/src/command.rs` shows why a claim alone is not enough:
  volume transfer snapshots, receives, then re-checks both current lease and
  current owner before writing the ownership fact.
- `legacy/mvp/deploy/src/coordinator.rs` and
  `legacy/mvp/deploy/src/state_machine.rs` show the real deploy boundary:
  inspect capacity, write decision, prepare/start participants, commit serving,
  wait for projection catch-up, drain, cleanup, and recover pending cleanup.
- `legacy/mvp/projection/src/reducer/key_expectation.rs` shows that evidence
  shape and fact-key shape must agree before data affects projections.

These are the design inputs. New Polis API names are only acceptable if they
make these cases easier to express while retaining the same safety properties.

## Requirements

- R1. Ployz product modules must not import `polis` directly. Direct Polis
  imports belong in adapters, composition, or root wiring.
- R2. Ployz product code must not call generic `record_evidence`,
  `terminalize`, or raw operation-store APIs.
- R3. Polis public types must pass the proof test: holding the value proves
  something, permits a narrower next action, and keeps product meaning outside
  Polis.
- R4. Operation lifecycle must use consuming transitions. An open operation can
  checkpoint, succeed, or fail; success and failure consume the open value.
- R5. Ployz should expose a product-facing `CommandContext<C>` or equivalent
  that is backed by Polis but hides generic evidence and terminal markers.
- R6. Claims must be guards, not records. Product ports should require typed
  guard values when they mutate guarded resources.
- R7. Product proof values must encode invariants in their constructors. A
  `UsableDomainCertificate` cannot be constructed unless it validates for the
  exact binding and policy.
- R8. Domain readiness must read current domain status before mutating so ready
  reuse, pending retry, and regression detection are explicit.
- R9. Retry/idempotency tests must actually replay the same operation or
  request across more than one attempt.
- R10. Deploy remains the owner of deploy terminalization. Domain readiness is
  a step inside deploy when a manifest has an HTTPS binding.
- R11. Polis must not become a workflow engine. It provides capability values;
  Ployz owns product orchestration.
- R12. The final review must include a code-shape pass that asks whether Ployz
  is now simpler and more Rust-idiomatic because of Polis.
- R13. Every Polis primitive introduced by this plan must trace to at least one
  concrete `legacy/mvp` pressure point or be deferred.
- R14. Claims must preserve the MVP lease realities: resource, holder, epoch,
  claim hash or equivalent fence, expiry, renewal/release semantics, and a
  protected mutation boundary.
- R15. Replay and idempotency APIs must account for MVP-style fact conflicts,
  duplicate writes, projection catch-up, and post-call invariant checks.

## Scope

### In Scope

- Replace the current Ployz `OperationRunner` facade with a Ployz command
  runner built on Polis open-operation capability values.
- Reshape Polis operation APIs around `OpenOperation<C>` and
  `ClosedOperation<C>` consuming transitions.
- Reshape Polis claim APIs around typed `ClaimGuard<R>` values with resource,
  fence, holder, epoch, and expiry semantics.
- Add Ployz-owned product proof values for domain readiness:
  `DomainClaim`, `UsableDomainCertificate`, `DomainServingActivation`, and
  `DomainReady`.
- Rewrite domain readiness so the product service reads like a proof chain and
  uses current status intentionally.
- Rewrite deploy HTTPS ensure so deploy calls domain readiness inside the
  deploy command context without giving domain readiness terminal authority
  over deploy.
- Add tests that prove replay, idempotency, boundary direction, and failure
  audience.

### Deferred

- Real DNS mismatch preflight for `domain add`. Keep the TODO comment from the
  domain-add plan.
- Real ACME production integration.
- NATS, Iroh, Corrosion, or other backend swaps.
- Separate Polis and Ployz repositories.
- A generic workflow engine, distributed queue processor, or background
  reconciler.
- Full migration, branch, promote, rollback, and machine lifecycle primitives.

### Non-Goals

- Do not polish the current `DomainReadinessService` into acceptability if the
  API still reads as framework scripting.
- Do not preserve source compatibility with the current spike.
- Do not create `polis-core`, `polis-messaging`, or similar taxonomy just to
  make the split look organized.
- Do not move product concepts into Polis to make Ployz shorter.

## Target Design

### Decision 1: Product Facade Above Capability Engine

Polis should expose capability values and backend traits. Ployz should expose a
smaller product-facing facade.

Polis-level concepts:

- `Authorized<A>`
- `OpenOperation<C>`
- `ClosedOperation<C>`
- `ClaimGuard<R>`
- `MutationReceipt<T, E>`
- `ProjectionSnapshot<T>`

Ployz-level concepts:

- `CommandContext<C>`
- `CommandRunner`
- `DomainClaim`
- `UsableDomainCertificate`
- `DomainReady`
- `DeployOutcome`

Rationale: `OpenOperation<C>` and `ClaimGuard<R>` are useful framework proofs,
but they should not leak into every product service signature. Ployz needs a
pleasant application API, not direct access to every primitive.

### Decision 2: Commands Own Operation Closure

Only the top-level command runner should close an operation. Nested product
steps return proof values and product failures.

Directional shape:

```rust
runner.run(DeployCommand { manifest }, |ctx, deploy| {
    let domain = domains.ensure_ready(ctx, deploy.https_binding())?;
    let runtime = runtime.activate(ctx, deploy.runtime_request())?;
    let serving = serving.commit(ctx, deploy.route_request(&domain))?;

    Ok(DeployOutcome {
        domain,
        runtime,
        serving,
    })
})
```

Rationale: `domain add` can be a top-level command, and domain readiness can
also be a deploy step. The operation close must belong to whichever command is
actually running.

### Decision 3: Product Proof Values Hide Raw Generic Guards

Domain code should not inspect `ClaimGuard<DomainResource>` directly. The
domain adapter turns a generic guard into a product proof value:

```rust
let claim: DomainClaim = ctx.claim(DomainResource::for_domain(&domain))?;
let cert: UsableDomainCertificate = certificates.ensure_usable(ctx, &claim, &domain)?;
let ready: DomainReady = serving.activate(ctx, &claim, cert)?;
```

Rationale: this keeps Polis generic while letting domain ports communicate
domain invariants. A `DomainClaim` can guarantee that the guarded resource
matches the domain, so product code never compares string identity manually.

### Decision 4: Typestate Only When It Carries Real Proof

Delete local sequencing types that only prove "we called the previous function."
Keep types that prove an invariant other ports can rely on.

Useful proof values:

- `DomainClaim`: guarded mutation authority for one domain resource.
- `UsableDomainCertificate`: certificate validates for hostname and policy.
- `DomainServingActivation`: serving accepted the certificate for the hostname.
- `DomainReady`: domain status is ready and usable by deploy routing.

Likely ceremony to remove:

- `DomainAttempt`
- `DomainClaimed`
- `DomainCertified`

Rationale: Rusty code is not "more types." It is types that prevent real bugs.

### Decision 5: Replay Is A First-Class Seam

Operation evidence can only accelerate replay if a product verifier confirms
the claimed invariant is still true. For domain readiness, replay should check
stored status and, when needed, verify certificate and serving activation before
returning ready.

Rationale: generic operation records are evidence, not truth. Ployz product
code owns the domain invariant.

## Implementation Units

### U0. Map Legacy MVP Pressures Before Designing APIs

**Goal:** Produce a short extraction map from concrete MVP code to the Polis
primitive that should replace or support it.

**Read:**

- `legacy/mvp/lease/src/lib.rs`
- `legacy/mvp/acme/src/lib.rs`
- `legacy/mvp/volume/src/command.rs`
- `legacy/mvp/deploy/src/coordinator.rs`
- `legacy/mvp/deploy/src/state_machine.rs`
- `legacy/mvp/projection/src/reducer/key_expectation.rs`
- relevant e2e contracts under `legacy/mvp/e2e/src/`

**Modify:**

- `docs/plans/2026-05-21-006-refactor-polis-proof-chain-api-plan.md`
  if the map changes the implementation units.
- Optionally add a focused design note under `docs/architecture.md` only if the
  extraction map uncovers a durable rule missing from architecture docs.

**Scenarios To Extract:**

- ACME HTTP-01 challenge ownership validates lease resource before presenting
  or clearing challenge facts.
- Volume transfer checks current owner, acquires lease, snapshots, receives,
  rechecks lease, rechecks owner, writes ownership fact, then verifies committed
  owner.
- Deploy restart recovery distinguishes pre-commit incomplete work from
  pending cleanup after serving commit.
- Projection rejects facts whose key and payload do not match.
- Duplicate fact writes and conflicts have different meanings.

**Acceptance:**

- Each proposed Polis type has a row mapping it to one or more MVP pressures.
- Any primitive without a concrete MVP pressure point is removed or explicitly
  deferred.
- The extraction map identifies which product invariant remains in Ployz for
  each primitive.

### U1. Lock The Desired Ployz API Shape First

**Goal:** Add tests or compile-time helper signatures that describe the product
API Ployz should expose before reshaping internals.

**Modify:**

- `crates/ployz/src/operation.rs`
- `crates/ployz/src/domain/mod.rs`
- `crates/ployz/src/deploy/mod.rs`

**Test:**

- `crates/ployz/src/operation.rs`
- `crates/ployz/src/domain/mod.rs`
- `crates/ployz/src/deploy/mod.rs`

**Scenarios:**

- A domain readiness service can be used with only `CommandContext`,
  `DomainName`, certificate port, serving port, and status port.
- A deploy service can call domain readiness as a step and still own deploy
  success/failure.
- Domain and deploy modules compile without direct `polis` imports.
- Product tests do not construct operation evidence or terminal markers.

**Acceptance:**

- The target API can be expressed in ordinary Rust helper functions without
  importing Polis in product modules.
- The test names describe product behavior, not framework mechanics.

### U2. Replace OperationRunner With CommandRunner

**Goal:** Make Ployz command execution own operation closure through a
product-facing context backed by Polis.

**Modify:**

- `crates/ployz/src/operation.rs`
- `crates/polis/src/operations.rs`
- adapter/composition files that currently implement `OperationPort`

**Test:**

- `crates/ployz/src/operation.rs`
- `crates/polis/src/operations.rs`
- `crates/ployz-e2e/src/scenarios/https_deploy.rs`
- `crates/ployz-e2e/src/scenarios/domain_add.rs`

**Scenarios:**

- Successful `CommandRunner::run` writes one terminal success marker and
  returns the product outcome.
- Failed `CommandRunner::run` writes one terminal failure marker with typed
  product failure evidence.
- A command closure can use `?` normally; failure recording happens once at the
  command boundary.
- Code cannot append product evidence after operation closure without going
  through a new operation.

**Acceptance:**

- `record_evidence` and `terminalize` are absent from domain and deploy
  product modules.
- `OpenOperation<C>::succeed(self, ...)` and `fail(self, ...)` consume the open
  value in Polis.

### U3. Introduce Typed Claim Guards And Product Claims

**Goal:** Move claims from inspectable records to guards that product ports can
trust.

**Modify:**

- `crates/polis/src/claims.rs`
- `crates/ployz/src/operation.rs`
- `crates/ployz/src/domain/mod.rs`

**Test:**

- `crates/polis/src/claims.rs`
- `crates/ployz/src/domain/mod.rs`
- `crates/ployz-e2e/src/scenarios/domain_add.rs`

**Scenarios:**

- Acquiring a claim for a domain resource returns a typed guard with fence,
  holder, epoch, expiry, and resource identity.
- Domain code receives a `DomainClaim`, not a raw generic claim record.
- Certificate and serving ports require `&DomainClaim` for guarded mutations.
- A stale or mismatched guarded resource is rejected before certificate or
  serving mutation.

**Acceptance:**

- No domain product code compares `claim.domain == request.domain`.
- Protected product ports require proof values in their signatures.

### U4. Rewrite Domain Readiness Around Proof Values

**Goal:** Make domain readiness a product service that reads current status,
mutates through typed proofs, and returns `DomainReady`.

**Modify:**

- `crates/ployz/src/domain/mod.rs`
- `crates/ployz-e2e/src/scenarios/domain_add.rs`

**Test:**

- `crates/ployz/src/domain/mod.rs`
- `crates/ployz-e2e/src/scenarios/domain_add.rs`

**Scenarios:**

- Existing ready status is reused only after certificate and serving invariants
  are still acceptable for the request policy.
- Pending status from a previous failed attempt causes a real second attempt
  with the same idempotency key/request, not a single-call assertion.
- Certificate issuance failure records pending or failed product status with an
  operator-visible reason and leaves the command boundary to close the
  operation.
- Serving activation failure does not record ready.
- Success records ready status and returns a `DomainReady` value usable by
  deploy.
- DNS mismatch remains a TODO comment and does not block this slice.

**Acceptance:**

- Domain readiness reads `DomainStatusPort::status` or removes the method if no
  current behavior needs it.
- Domain readiness does not close operations.
- The core happy path is a short proof chain, not a transaction script.

### U5. Make Deploy HTTPS Ensure Use Domain Readiness As A Step

**Goal:** Deploy with HTTPS binding uses the same domain readiness service while
deploy remains the top-level command.

**Modify:**

- `crates/ployz/src/deploy/mod.rs`
- `crates/ployz-e2e/src/scenarios/https_deploy.rs`
- `crates/ployz-e2e/src/scenarios/coordinator_restart.rs`

**Test:**

- `crates/ployz/src/deploy/mod.rs`
- `crates/ployz-e2e/src/scenarios/https_deploy.rs`
- `crates/ployz-e2e/src/scenarios/coordinator_restart.rs`

**Scenarios:**

- Deploy manifest with HTTPS binding ensures domain readiness before reporting
  deploy success.
- Certificate failure fails the deploy and does not publish a successful route.
- Serving activation failure fails the deploy and preserves previous valid
  serving state.
- Coordinator restart after pending domain readiness can retry and complete
  without duplicate terminal markers.
- Deploy outcome includes enough typed evidence for API/CLI surfaces without
  exposing raw Polis evidence.

**Acceptance:**

- Deploy code reads as product orchestration: ensure domain, activate runtime,
  commit serving, return outcome.
- Domain readiness failure maps into deploy failure at the deploy boundary.

### U6. Move Product Evidence Encoding To The Adapter Boundary

**Goal:** Product modules produce typed events; adapters encode them into Polis
records.

**Modify:**

- `crates/ployz/src/operation.rs`
- adapter/composition files that persist operation evidence
- `crates/polis/src/operations.rs`

**Test:**

- `crates/ployz/src/operation.rs`
- `crates/polis/src/operations.rs`
- relevant e2e scenario fakes

**Scenarios:**

- Product code checkpoints `DomainEvidence` or `DeployEvidence`, not generic
  payload bytes.
- Evidence encoding preserves product kind, product phase, timestamp, and safe
  summary fields.
- Secret material, private keys, and raw certificate keys cannot be placed into
  generic evidence through the product helper API.
- Replay code can read evidence but still verifies product invariants before
  returning success.

**Acceptance:**

- Opaque encoding is below the Ployz product module boundary.
- Evidence is useful for operators without becoming the source of truth.

### U7. Remove Fake Typestate, Dead APIs, And Boundary Leaks

**Goal:** Delete the abstractions that do not prove real invariants and enforce
the intended dependency boundary.

**Modify:**

- `crates/ployz/src/domain/mod.rs`
- `crates/ployz/src/operation.rs`
- `crates/ployz/src/deploy/mod.rs`
- `docs/architecture.md`
- `README.md` if public crate descriptions changed

**Test:**

- `crates/ployz/src/domain/mod.rs`
- `crates/ployz/src/deploy/mod.rs`
- boundary checks through `rg` during verification

**Scenarios:**

- Product modules have no direct `polis::` imports.
- Domain and deploy modules have no `record_evidence` or `terminalize` calls.
- Unused status, typestate, and runner APIs are removed.
- Docs describe Polis as capability machinery for Ployz, not a workflow engine.

**Acceptance:**

- The public API surface is smaller than the spike API.
- Names reflect product concepts in Ployz and capability concepts in Polis.

### U8. Run The Code-Shape Review Gate

**Goal:** Treat elegance and Rust idiom as a required outcome, not a vibe check.

**Review Questions:**

- Does the domain readiness happy path fit in one small function without
  manual operation bookkeeping?
- Does deploy read as a product primitive rather than a framework transaction?
- Does every retained type prove something a later call relies on?
- Are failures classified by audience and handled at the right boundary?
- Could a new product primitive follow this pattern without learning Polis
  storage internals?

**Acceptance:**

- Review findings are either fixed or explicitly deferred with rationale.
- The plan is not complete if tests pass but product code still reads like a
  hand-written transaction script.

## Verification

Run targeted tests during the relevant implementation unit, then finish with:

```sh
just check
cargo clippy --workspace --all-targets -- -D warnings
```

Boundary checks:

```sh
rg "use polis|polis::" crates/ployz/src --glob '!adapters/**' --glob '!composition.rs'
rg "record_evidence|terminalize" crates/ployz/src/domain crates/ployz/src/deploy
rg "claim\\..*domain|domain.*==.*claim" crates/ployz/src/domain
```

Expected result:

- The first search returns only adapter/composition hits.
- The second search returns no product-module hits.
- The third search returns no manual claim/domain identity checks.

## Risks

| Risk | Mitigation |
| --- | --- |
| Polis becomes a workflow engine | Keep command runner in Ployz; Polis only supplies capability values. |
| Product facade duplicates Polis too much | Allow thin Ployz wrappers only where they hide generic framework mechanics from product modules. |
| Typestate returns as ceremony | Keep only types that prove invariants consumed by later ports. |
| Replay semantics become fake again | Require a multi-attempt test with same request/idempotency and explicit invariant verification. |
| Operation evidence becomes truth | Treat evidence as replay acceleration and operator context; product verifiers own truth. |
| Scope expands into backend design | Defer backend swaps and repo split until Ployz code shape is proven. |
| Clean-room Polis drifts from MVP reality | Start with the legacy MVP pressure map and reject primitives that do not simplify a real MVP case. |

## Completion Criteria

- Ployz domain and deploy code read as short typed proof chains.
- Domain and deploy modules do not import Polis directly.
- Product modules do not manually terminalize operations or append generic
  evidence.
- Domain readiness uses or removes current status intentionally.
- Retry/idempotency tests replay real attempts.
- Deploy HTTPS ensure calls domain readiness as a step and owns deploy closure.
- Polis public APIs are capability values with consuming transitions where
  lifecycle closure matters.
- Every retained Polis primitive maps back to concrete `legacy/mvp` behavior.
- Code-shape review agrees Polis is making Ployz simpler, not merely moving
  complexity sideways.
