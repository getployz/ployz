---
title: "refactor: Redesign Polis Around Typed Capability Values"
type: refactor
status: active
date: 2026-05-21
origin: chat
depends_on:
  - docs/brainstorms/2026-05-21-polis-ployz-boundary-requirements.md
  - docs/plans/2026-05-21-003-refactor-polis-ployz-root-api-boundary-plan.md
  - docs/plans/2026-05-21-004-feat-domain-add-https-readiness-plan.md
---

# refactor: Redesign Polis Around Typed Capability Values

## Summary

Reset the Polis/Ployz root API boundary around typed capability values rather
than framework nouns. The current domain HTTPS readiness spike proved behavior,
but the resulting Ployz code reads like a hand-written transaction script:
operation evidence, terminalization, status writes, claims, certificate checks,
and serving activation are sequenced inline.

The new target is:

- Polis exposes values that prove something and permit a narrower next action:
  `Authorized<A>`, `OpenOperation<C>`, `ClaimGuard<R>`,
  `MutationReceipt<T>`, and `ProjectionSnapshot<T>`.
- Ployz owns product runners and product status mapping.
- Product domains consume proven values instead of calling generic framework
  ports directly.
- Domain HTTPS readiness is rebuilt as typed product transitions, then deploy
  calls it as a step inside the deploy operation without letting domain
  readiness terminalize deploy.

This plan intentionally treats the current `crates/ployz/src/domain/` spike as
disposable evidence, not a code shape to polish.

---

## Problem Frame

The root boundary plan says Polis should keep distributed control-plane
mechanics out of Ployz product code while avoiding a workflow engine (see
origin: `docs/plans/2026-05-21-003-refactor-polis-ployz-root-api-boundary-plan.md`).
The domain-add spike exposed that the current APIs do not achieve that.

The failure mode is not dependency direction. Polis stayed product-neutral, but
Ployz still had to manually juggle framework mechanics:

- `DomainAddRequest` became an execution envelope with operation id,
  idempotency, authority, fence, and deadline instead of a product request.
- `DomainReadinessEngine` coupled domain status and operation bookkeeping
  through `DomainStatusPort + OperationPort`.
- Failure paths could not use normal `?` because every error also needed status
  failure evidence and terminalization.
- `DomainClaim { domain }` did not prove guarded mutation authority, so product
  code compared the claim back to the request manually.
- `ensure_usable_certificate` returned a general certificate value, forcing
  duplicate validation after a method named `ensure`.
- Domain readiness terminalized an operation even though deploy needs to call
  domain readiness inside a larger deploy operation.

The reset should make the correct design hard to misuse. Holding a Polis value
should prove something useful and enable only the next narrower action.

---

## Requirements

- R1. Polis APIs must be value-oriented. Every public Polis type should prove a
  fact, narrow the caller's next permitted action, or carry typed evidence of a
  bounded result.
- R2. Polis must remain product-neutral. It must not contain domain, deploy,
  certificate, serving, runtime, volume, or route concepts.
- R3. Ployz product modules must not manually append generic evidence,
  terminalize generic operations, inspect raw claim resource identity, or build
  opaque evidence payloads.
- R4. Operation lifecycle must be modeled through consuming transitions:
  `OpenOperation<C>` can checkpoint, fail, or succeed; success/failure consumes
  the open operation and returns a closed value.
- R5. Claims must become typed guards. A product mutation protected by a claim
  should accept `ClaimGuard<R>`, not a record that product code must inspect.
- R6. Product evidence must be typed at the Ployz boundary. Polis may store
  encoded opaque evidence, but Ployz code should call typed methods such as
  `checkpoint(DomainEvidence::CertificateIssued)`.
- R7. Product runners belong in Ployz, not Polis. Polis provides operation
  capability values; Ployz maps product failures into product status and
  operator-facing results.
- R8. Domain HTTPS readiness must become a product service and domain state
  model, not an operation runner. It may record domain status, but it must not
  terminalize the caller's operation.
- R9. Deploy must call the same domain readiness service for HTTPS bindings
  while deploy remains the owner of deploy terminalization.
- R10. The redesigned slice must be judged by code shape as well as tests:
  Ployz should read as small product orchestration and typed transitions.

---

## Scope

### In Scope

- Redesign Polis operation, authority, claim, call receipt, and projection
  surfaces around typed capability values.
- Add adapter-facing traits where needed to persist or encode the capability
  values without leaking storage mechanics into Ployz product code.
- Replace the current domain readiness spike with a product-only API and typed
  domain transitions.
- Rework deploy to call domain readiness inside deploy's open operation.
- Add tests that prove invalid sequencing is impossible or visibly awkward.
- Add a design-review checkpoint that specifically evaluates Ployz readability
  and whether Polis is carrying the right complexity.

### Deferred

- Real DNS mismatch preflight.
- Real ACME, serving, NATS, or daemon adapters.
- Separate `polis` and `ployz` repositories.
- Generic workflow engine, background reconciler, or broad queue processor.
- Full migration, branch, promote, rollback, or machine lifecycle primitives.

### Non-Goals

- Do not make Polis a product framework or public operator surface.
- Do not hide product workflows inside Polis operation runners.
- Do not add abstractions for hypothetical backends.
- Do not preserve the current `DomainReadinessEngine` API for compatibility.

---

## Target API Direction

### Polis Capability Values

Polis should move from service nouns toward proven values:

| Capability | Proves | Enables |
| --- | --- | --- |
| `Authorized<A>` | Actor `A` is authorized for a scope at a grant epoch | Starting scoped operations, authorizing records or calls |
| `OpenOperation<C>` | Command `C` owns an idempotent open operation and mutation context | Checkpoints, claims, bounded calls, final success/failure |
| `ClosedOperation<C>` | Operation has one terminal marker | Read/report only; no more evidence appends |
| `ClaimGuard<R>` | Resource kind `R` is currently guarded by an advisory claim and fence token | Calling protected product ports for that resource |
| `MutationReceipt<T, E>` | Bounded call produced a durable success `T` or typed failure `E` | Replay-safe product verification |
| `ProjectionSnapshot<T>` | Projection `T` was read with freshness metadata | Product decision using explicit freshness |

The type test for new Polis APIs:

1. Does holding this value prove something?
2. Does it permit a narrower next action?
3. Can invalid sequencing be made impossible or visibly awkward?
4. Is product meaning outside the type?

If the answer is no, the type is probably a framework noun and should stay
private or be folded into an adapter.

### Operation Shape

Directional shape:

```rust
let authorized = authority.authorize(actor, scope)?;
let mut operation = operations.start(command, authorized)?;
operation.checkpoint(ProductEvidence::Started)?;

let result = product.run(operation.context())?;

operation.succeed(result.summary())?;
```

Key decisions:

- `OpenOperation<C>::succeed(self, evidence)` and
  `OpenOperation<C>::fail(self, evidence)` consume the open value.
- Product code should not call `terminalize(&operation_id, marker)` directly.
- Generic evidence storage remains Polis-owned, but product-facing evidence is
  typed and encoded by adapters.
- Replayed operations must not be trusted as product success unless the Ployz
  verifier confirms the domain invariant.

### Claim Shape

Directional shape:

```rust
pub enum DomainMutation {}

let resource = ResourceId::<DomainMutation>::parse("domain:app.example.com")?;
let guard: ClaimGuard<DomainMutation> = claims.acquire(&operation, resource)?;
```

Key decisions:

- `ClaimGuard<R>` carries a typed resource id, fence token, holder, epoch, and
  expiry.
- Product code should not compare `claim.domain == request.domain`.
- Protected product ports accept the guard type that matches their mutation
  resource.
- Claims remain advisory. Real exclusivity is still enforced at the protected
  resource's mutation boundary.

### Domain Readiness Shape

Directional product shape:

```rust
let ready = domains.ensure_ready(
    operation.context(),
    DomainAdd {
        domain,
        certificate_policy,
    },
)?;
```

Domain readiness owns:

- `DomainName`
- `DomainAdd`
- `DomainStatus`
- `DomainFailure`
- `DomainReady`
- `DomainResource`
- `DomainEvidence`
- typed transitions such as `DomainAttempt -> DomainClaimed ->
  DomainCertified -> DomainReady`

Domain readiness does not own:

- operation id or idempotency key fields in the product request;
- authority envelope fields;
- operation terminalization;
- deploy success/failure mapping.

### Deploy Shape

Directional deploy shape:

```rust
let mut operation = operations.start(deploy_command, authorized)?;

let ready = domains.ensure_ready(operation.context(), manifest.domain_add())?;
let runtime = runtime.activate(operation.context(), manifest.runtime_request())?;
let serving = serving.commit(operation.context(), manifest.serving_request(&ready))?;

operation.succeed(DeployEvidence::Succeeded)?;
```

Deploy remains responsible for:

- manifest interpretation;
- runtime activation;
- route commit;
- serving activation verification for deploy's route;
- deploy terminalization and deploy-facing failures.

Domain readiness remains responsible for:

- domain status;
- certificate usability;
- serving certificate activation;
- returning `DomainReady` as a product value.

---

## Implementation Units

### U1. Introduce Polis Capability Value Types

**Goal:** Add the capability values without changing product behavior yet.

**Active files:**
- Modify: `crates/polis/src/authority.rs`
- Modify: `crates/polis/src/operations.rs`
- Modify: `crates/polis/src/claims.rs`
- Modify: `crates/polis/src/calls.rs`
- Modify: `crates/polis/src/projections.rs`
- Modify: `crates/polis/src/lib.rs`
- Test: same files' unit test modules

**Approach:**
- Add `Authorized<A>` as the successful authority proof returned from an
  authorization decision.
- Add marker-typed `Command<C>` or `CommandKind<C>` identity for operation
  ownership.
- Add `OpenOperation<C>` and `ClosedOperation<C>` with consuming terminal
  transitions.
- Add marker-typed `ResourceId<R>` and `ClaimGuard<R>`.
- Add `MutationReceipt<T, E>` as a typed wrapper over bounded call results.
- Add `ProjectionSnapshot<T>` carrying typed view plus freshness.
- Keep existing low-level store traits where useful, but make them adapter
  plumbing rather than the preferred product-facing surface.

**Test scenarios:**
- A denied or unknown authority decision cannot produce `Authorized<A>`.
- `OpenOperation<C>::succeed(self, ...)` consumes the open operation.
- `OpenOperation<C>::fail(self, ...)` consumes the open operation.
- `ClaimGuard<DomainResource>` is not accepted where
  `ClaimGuard<VolumeResource>` is required.
- `MutationReceipt<T, E>` preserves replayed failure as a typed failure, not a
  display string.
- `ProjectionSnapshot<T>` preserves freshness and does not treat unknown as
  fresh.

---

### U2. Add Ployz Operation Runner

**Goal:** Move operation lifecycle choreography out of product engines while
keeping product failure/status mapping in Ployz.

**Dependencies:** U1

**Active files:**
- Modify: `crates/ployz/src/operation.rs`
- Modify: `crates/ployz/src/adapters/polis.rs`
- Modify: `crates/ployz/src/composition.rs`
- Test: `crates/ployz/src/operation.rs`

**Approach:**
- Define a Ployz-owned `OperationRunner<C, E>` that starts an operation through
  Polis-backed capabilities and runs a closure with a product mutation context.
- The runner maps closure success to `OpenOperation<C>::succeed(...)`.
- The runner maps closure failure through a product-provided failure encoder
  and `OpenOperation<C>::fail(...)`.
- Product modules provide typed evidence and failure summaries; they do not
  call generic evidence or terminal APIs directly.

**Test scenarios:**
- Successful closure terminalizes once with typed success evidence.
- Failed closure terminalizes once with typed failure evidence.
- A closure cannot append evidence after the runner consumes the open operation.
- Operation backend failure maps to `PrimitiveFailure` without string parsing.
- Product failure mapping remains in Ployz, not Polis.

---

### U3. Replace Domain Spike With Typed Domain Model

**Goal:** Rebuild domain readiness around product request/state transitions,
not an execution envelope.

**Dependencies:** U1, U2

**Active files:**
- Replace or heavily modify: `crates/ployz/src/domain/mod.rs`
- Test: `crates/ployz/src/domain/mod.rs`

**Approach:**
- Replace `DomainAddRequest` with product-only `DomainAdd`.
- Remove operation id, idempotency, authority, fence, and deadline fields from
  the product request.
- Add `DomainResource` marker and require `ClaimGuard<DomainResource>` at
  protected certificate/serving mutation ports.
- Replace `DomainClaim { domain }` with typed claim guards.
- Add `UsableDomainCertificate` so certificate usability is encoded in the
  return type and not rechecked in the service.
- Model the product transition as typed states:
  - `DomainAttempt`
  - `DomainClaimed`
  - `DomainCertified`
  - `DomainReady`
- Keep domain status recording product-owned and separate from operation
  terminalization.
- Keep DNS preflight as a TODO/comment only.

**Test scenarios:**
- Invalid domain is rejected before claim acquisition.
- Certificate port cannot be called without `ClaimGuard<DomainResource>`.
- A certificate with hostname mismatch, unsafe material, revoked freshness, or
  short safety window cannot become `UsableDomainCertificate`.
- Serving activation failure records product failure and does not record ready.
- Success records domain ready without terminalizing the caller's operation.
- Private key material cannot appear in status, evidence, or errors.

---

### U4. Rework Deploy To Use Domain Service Inside Deploy Operation

**Goal:** Make deploy call the domain readiness service as a step, while deploy
owns the deploy operation lifecycle.

**Dependencies:** U2, U3

**Active files:**
- Modify: `crates/ployz/src/deploy/mod.rs`
- Modify: `crates/ployz-e2e/src/scenarios/https_deploy.rs`
- Test: `crates/ployz-e2e/src/scenarios/https_deploy.rs`

**Approach:**
- Deploy receives or constructs a product `DomainAdd` from the HTTPS binding.
- Deploy calls `domains.ensure_ready(ctx, domain_add)` inside the deploy
  operation runner.
- Deploy uses the returned `DomainReady` value when constructing serving route
  state.
- Domain readiness must not terminalize deploy.
- Deploy maps `DomainFailure` into deploy-facing failure types without
  collapsing domain infrastructure failures into runtime failures.

**Test scenarios:**
- HTTPS deploy calls domain readiness before runtime activation.
- Domain readiness failure fails deploy before runtime or route mutation.
- Domain readiness success allows runtime activation and serving commit.
- Deploy terminalizes once, even though domain readiness records product domain
  status.
- Deploy failure preserves a domain-readiness failure class.

---

### U5. Add Product Evidence Encoders

**Goal:** Keep product evidence typed while Polis stores generic opaque
evidence.

**Dependencies:** U1, U2, U3, U4

**Active files:**
- Modify: `crates/ployz/src/operation.rs`
- Modify: `crates/ployz/src/domain/mod.rs`
- Modify: `crates/ployz/src/deploy/mod.rs`
- Modify: `crates/ployz/src/adapters/polis.rs`
- Test: `crates/ployz/src/operation.rs`

**Approach:**
- Add product evidence enums such as `DomainEvidence` and `DeployEvidence`.
- Add Ployz-owned encoders that convert typed evidence to opaque bytes or
  minimal structured metadata for Polis.
- Keep encoders adapter-facing; ordinary product code should pass typed
  evidence values.
- Do not expose raw `Vec<u8>` evidence construction in product feature modules.

**Test scenarios:**
- Domain evidence encodes without exposing secrets.
- Deploy evidence encodes without exposing secrets.
- Product code can checkpoint typed evidence without importing Polis evidence
  variants.
- Unknown encoder failure maps to a structured primitive failure.

---

### U6. Rewrite Domain Add Acceptance Tests Around Shape

**Goal:** Preserve behavioral coverage while adding code-shape assertions that
would have caught the current spike.

**Dependencies:** U3, U4, U5

**Active files:**
- Modify: `crates/ployz-e2e/src/scenarios/domain_add.rs`
- Modify: `crates/ployz-e2e/src/scenarios/https_deploy.rs`
- Test: same files

**Approach:**
- Keep existing scenarios for ready success, certificate safety-window failure,
  serving activation failure, and retry verification.
- Add assertions that domain readiness does not terminalize operation state
  when used as a deploy step.
- Add assertions that deploy terminalizes once.
- Add compile-time-oriented tests where practical by requiring typed claim
  guards in helper signatures.

**Test scenarios:**
- `domain_add_records_ready_without_generic_terminalization`
- `domain_add_certificate_safety_window_failure_is_visible`
- `domain_add_serving_activation_failure_is_not_ready`
- `https_deploy_uses_domain_ready_before_runtime_activation`
- `https_deploy_terminalizes_once_after_route_activation`
- `domain_claim_guard_type_prevents_wrong_resource_use`

---

### U7. Update Architecture And Boundary Docs

**Goal:** Record the capability-value rule so future slices do not drift back
to framework nouns.

**Dependencies:** U1-U6

**Active files:**
- Modify: `README.md`
- Modify: `docs/architecture.md`
- Modify: `docs/plans/2026-05-21-003-refactor-polis-ployz-root-api-boundary-plan.md`
- Modify: `docs/plans/2026-05-21-004-feat-domain-add-https-readiness-plan.md`
- Create or modify: `docs/plans/2026-05-21-005-refactor-polis-capability-values-plan.md`

**Approach:**
- Add the Polis type test:
  - proves something;
  - permits a narrower next action;
  - makes invalid sequencing impossible or awkward;
  - carries no product meaning.
- Mark the domain-add plan as a spike if implementation replaces its code
  shape.
- Document that product runners live in Ployz.
- Document that domain readiness is a deploy step, not an operation
  terminalizer.

**Test scenarios:**
- Documentation only. No code tests.

---

## Verification

Targeted checks:

```bash
cargo test -p polis
cargo test -p ployz operation
cargo test -p ployz domain
cargo test -p ployz-e2e domain_add
cargo test -p ployz-e2e https_deploy
```

Full gates:

```bash
just check
cargo clippy --workspace --all-targets -- -D warnings
```

Design review gate:

- Review `crates/ployz/src/domain/mod.rs` and confirm domain code reads as
  typed product transitions, not operation lifecycle choreography.
- Review `crates/ployz/src/deploy/mod.rs` and confirm deploy owns deploy
  terminalization while using domain readiness as a product step.
- Review `crates/polis/src/*` and confirm public types are capability values,
  not product-shaped framework nouns.

---

## Risks

| Risk | Mitigation |
| --- | --- |
| Capability generics become abstraction theater | Only add a capability when at least one product slice consumes it. Keep marker types small and concrete. |
| Polis turns into a workflow engine | Keep runners in Ployz. Polis owns values and stores, not product workflow sequencing. |
| Product code still sees framework plumbing | Add tests and review gates that flag direct generic evidence, terminalization, raw claim inspection, and raw `Vec<u8>` evidence in product modules. |
| Domain readiness loses standalone command semantics | Standalone `domain add` should be implemented as a Ployz operation runner around the same domain service, not as a separate engine. |
| Deploy/domain operation boundaries blur again | Domain readiness may record domain status, but only deploy terminalizes deploy. Tests must assert this. |
| Typed guards make tests noisy | Provide compact in-process fake helpers that construct authorized operations and guards without hiding the public API shape. |

---

## Completion Criteria

- Polis exposes `Authorized`, `OpenOperation`, `ClosedOperation`,
  `ClaimGuard`, `MutationReceipt`, and `ProjectionSnapshot` or equivalent
  capability values.
- Product modules no longer call generic `record_evidence` or `terminalize`
  APIs directly.
- Domain readiness accepts a product-only request and returns a product
  `DomainReady` value.
- Domain certificate and serving ports require a typed domain claim guard.
- Usable certificates are represented by a type that encodes the usability
  invariant.
- Deploy calls domain readiness inside deploy's operation and remains the only
  owner of deploy terminalization.
- Domain add and HTTPS deploy scenarios pass.
- `just check` and workspace clippy pass.
- A code-shape review explicitly confirms Ployz is simpler because Polis now
  provides proven capability values.
