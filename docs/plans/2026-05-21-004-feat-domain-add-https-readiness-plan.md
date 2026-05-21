---
title: "feat: Add Domain HTTPS Readiness Primitive"
type: feat
status: completed
date: 2026-05-21
origin: chat
depends_on:
  - docs/plans/2026-05-21-003-refactor-polis-ployz-root-api-boundary-plan.md
superseded_by:
  - docs/plans/2026-05-21-005-refactor-polis-capability-values-plan.md
---

# feat: Add Domain HTTPS Readiness Primitive

## Summary

Add a Ployz product primitive for making a domain HTTPS-ready for the cluster:

```bash
ployz domain add nickpotts.com.au
```

The command should mean: validate the domain, obtain or activate usable TLS
material, verify serving activation, and record the domain as ready for future
deploy HTTPS bindings.

This primitive should later become deploy's HTTPS step. Deploy should not own
ACME choreography directly; it should call the same domain readiness operation
and fail if readiness cannot be established.

The deeper goal is to create a concrete slice that can be reviewed after
implementation for code shape: whether the Ployz product code reads clearly,
where it still feels awkward, and whether Polis is carrying enough foundational
complexity to keep Ployz simple.

Post-implementation review found that the first implementation worked as a
behavioral spike but did not meet the code-shape goal. It was superseded by the
capability-value reset plan, which rebuilds domain readiness as typed product
transitions and moves operation lifecycle choreography out of the domain model.

---

## Problem Frame

The current root proof has deploy ensuring a certificate through ACME-facing
ports. That proves the Polis/Ployz boundary, but it still leaves certificate
readiness somewhat buried inside deploy.

`domain add` is a better product semantic test:

- it is useful as a standalone command;
- it exercises authority, operation context, claims/fences, serving activation,
  certificate issuance, and product status;
- it can be reused by deploy without making deploy understand ACME internals.

This should stay a Ployz product primitive. Polis must not learn domain, ACME,
certificate, or serving concepts.

---

## Requirements

- R1. Ployz owns domain readiness semantics: hostname validation, certificate
  usability, serving activation, and operator-facing status.
- R2. Polis remains product-neutral. Direct Polis use stays in adapters and
  composition code only.
- R3. `domain add <hostname>` fails before recording ready when certificate
  issuance, certificate usability, or serving activation cannot be proven.
- R4. DNS mismatch detection is deferred. The API should leave room for a
  future DNS preflight without adding DNS ports or acceptance tests in this
  slice.
- R5. The first implementation may use fakeable/in-process certificate,
  serving, status, and claim ports, but the API must preserve the real product
  contract.
- R6. Certificate issuance or activation succeeds only when the certificate is
  usable for the exact hostname, material is protected, revocation freshness is
  known enough, the safety window is satisfied, and serving activation is
  acknowledged.
- R7. Private key material never appears in operation evidence, errors, status,
  logs, or test snapshots.
- R8. `domain add` records durable product status as ready, failed, or pending
  with structured reasons.
- R9. The operation is idempotent: retrying the same operation either returns
  the same ready result or resumes after verifying certificate, serving, and
  status invariants.
- R10. Deploy with an HTTPS binding later calls this domain readiness primitive.
  Deploy success requires HTTPS readiness; certificate renewal is still out of
  deploy scope.

---

## Scope

### In Scope

- Add a Ployz domain module and in-process product acceptance tests.
- Define fakeable product ports for domain status, domain claims, certificate
  ensure, and serving activation.
- Prove `domain add` semantics through in-process tests:
  - certificate safety-window failure fails visibly;
  - serving activation failure does not record ready;
  - retry after a checkpoint verifies invariants before success.
- Refactor deploy to call the domain readiness primitive for HTTPS readiness.
- Run a focused post-implementation design review of the resulting Ployz and
  Polis boundary.

### Deferred

- DNS preflight that proves the hostname already points at this cluster before
  attempting issuance.
- Real ACME issuer adapter.
- Real serving role adapter.
- CLI binary and external API wiring.
- Renewal scheduling or certificate maintenance roles.
- Wildcard certificates, DNS-01, multi-provider DNS automation, and automatic
  DNS record creation.
- Real daemon/substrate E2E runner.

### Non-Goals

- Do not turn domain readiness into a reconciler.
- Do not add domain concepts to Polis.
- Do not make deploy responsible for ACME challenge flow.

---

## Product Semantics

`ployz domain add nickpotts.com.au` is foreground work with a visible result.

Expected success path:

1. Validate `nickpotts.com.au` as a domain name.
2. Authorize the operation for the current principal and scope.
3. Claim the domain/certificate/challenge mutation resource.
4. Ensure a usable certificate for the exact hostname.
5. Activate certificate material on serving roles.
6. Verify serving activation and certificate usability.
7. Record domain status as ready.
8. Terminalize operation success.

TODO: Add DNS preflight later. Before attempting one-shot issuance, `domain add`
should probe the hostname and prove it points at this cluster. DNS mismatch,
unknown DNS, and unknown ingress should become structured foreground failures
when that slice is explicitly in scope.

---

## API Direction

Add `crates/ployz/src/domain/mod.rs`.

Core product types:

- `DomainName`
- `DomainReadiness`
- `DomainReady`
- `DomainStatus`
- `DomainFailure`
- `DomainAddRequest`
- `DomainAddOutcome`

Core ports:

- `DomainClaimPort`: claim/fence the domain/certificate/challenge resource.
- `DomainCertificatePort`: ensure certificate usability for a hostname.
- `DomainServingPort`: activate/verify certificate serving for the hostname.
- `DomainStatusPort`: read and write product domain readiness status.
- `OperationPort`: reused from `ployz::operation`.

Directional engine shape:

```rust
DomainReadinessEngine::ensure_ready(request) -> Result<DomainAddOutcome, DomainFailure>
```

This engine must read as product code:

```rust
let claim = claims.claim_domain(context, domain)?;
let certificate = certificates.ensure_usable(context, claim, domain)?;
serving.activate_certificate(context, domain, certificate)?;
status.record_ready(context, domain, certificate)?;
```

The actual implementation should choose simpler names where appropriate, but
the dependency direction should hold: domain code depends on Ployz-owned ports,
not Polis.

---

## State Ownership

| State | Owner | Mutators | Failure audience |
| --- | --- | --- | --- |
| Domain readiness status | Ployz domain | `domain add`, deploy HTTPS ensure | CLI/API caller and domain status surface |
| Certificate material status | Ployz ACME/serving | certificate adapter and serving activation | Caller and certificate status |
| Operation evidence | Ployz operation adapter backed by Polis | domain readiness engine | caller and operator status |
| Claim/fence token | Ployz domain adapter backed by Polis claims | domain readiness engine | stale claim failure before mutation |

---

## Implementation Units

### U1. Define Domain Product Model and Status

**Goal:** Add Ployz domain types and readiness semantics without touching deploy.

**Active files:**
- Modify: `crates/ployz/src/lib.rs`
- Create: `crates/ployz/src/domain/mod.rs`
- Test: `crates/ployz/src/domain/mod.rs`

**Approach:**
- Add fallible `DomainName::parse`.
- Add `DomainStatus` and `DomainReadiness` types that distinguish ready,
  pending, and failed states.
- Add structured `DomainFailure` variants for invalid domain, certificate
  unusable, serving activation failed, stale claim, and unknown readiness.
- Leave an explicit TODO near the readiness model for future DNS preflight
  variants.

**Test scenarios:**
- Empty or invalid domain is rejected.
- Domain readiness status preserves ready, pending, and failed reasons.
- Domain failures are structured and do not expose private material.

---

### U2. Add Domain Readiness Ports and Engine

**Goal:** Add the product orchestration surface for `domain add` using only
Ployz-owned ports.

**Dependencies:** U1

**Active files:**
- Modify: `crates/ployz/src/domain/mod.rs`
- Test: `crates/ployz/src/domain/mod.rs`

**Approach:**
- Define fakeable ports for claim, certificate ensure, serving activation,
  status writes, and operation evidence.
- `DomainReadinessEngine::ensure_ready` performs:
  - existing-ready status short path after verification;
  - claim/fence;
  - certificate ensure;
  - serving activation;
  - ready status write;
  - terminal success.
- Domain status is product status, not generic operation evidence. Operation
  evidence can carry opaque checkpoints/failures, but the domain module owns
  the meaning.

**Test scenarios:**
- Certificate unusable does not write ready.
- Serving activation unknown/failure does not write ready.
- Success writes ready and terminalizes operation.
- Cleanup/secrets are not represented in evidence.

---

### U3. Add In-Process Product Acceptance Scenarios

**Goal:** Prove the user-facing command semantics without claiming real
daemon/substrate E2E.

**Dependencies:** U2

**Active files:**
- Create: `crates/ployz-e2e/src/scenarios/domain_add.rs`
- Modify: `crates/ployz-e2e/src/scenarios/mod.rs`
- Test: `crates/ployz-e2e/src/scenarios/domain_add.rs`

**Approach:**
- Keep tests in-process with fake ports, matching the current root proof style.
- Use scenario names that describe product behavior, not adapter internals.

**Test scenarios:**
- `domain_add_issues_and_records_ready`
- `domain_add_certificate_safety_window_failure_is_visible`
- `domain_add_serving_activation_failure_is_not_ready`
- `domain_add_retry_after_checkpoint_verifies_ready_invariants`

---

### U4. Make Deploy Use Domain HTTPS Readiness

**Goal:** Move deploy's HTTPS certificate ensure step onto the domain readiness
primitive.

**Dependencies:** U2, U3

**Active files:**
- Modify: `crates/ployz/src/deploy/mod.rs`
- Modify: `crates/ployz-e2e/src/scenarios/https_deploy.rs`
- Test: `crates/ployz-e2e/src/scenarios/https_deploy.rs`

**Approach:**
- Replace deploy's direct certificate ensure path with a deploy-owned port such
  as `DomainReadinessPort`.
- Deploy remains responsible for manifest interpretation, runtime activation,
  serving route commit, and deploy result classification.
- Domain readiness remains responsible for certificate and serving certificate
  readiness.

**Test scenarios:**
- HTTPS deploy calls domain readiness before runtime activation.
- Domain readiness success allows deploy to continue.
- Domain readiness failure maps to structured deploy certificate/domain failure.

---

### U5. Add CLI/API Shape as a Design Stub

**Goal:** Define the external command shape without building a full CLI.

**Dependencies:** U2

**Active files:**
- Modify: `docs/architecture.md`
- Modify: `README.md`
- Create or modify: `docs/plans/2026-05-21-004-feat-domain-add-https-readiness-plan.md`

**Approach:**
- Document intended command:
  - `ployz domain add <hostname>`
  - future structured output fields: domain, certificate status, serving
    activation, readiness state.
- Keep CLI implementation deferred until there is a command shell in the root
  rewrite.

**Test scenarios:**
- Documentation only in this unit. No code tests.

---

## Verification

Each implementation slice should pass:

```bash
just check
cargo clippy --workspace --all-targets -- -D warnings
```

Domain-specific targeted tests:

```bash
cargo test -p ployz domain
cargo test -p ployz-e2e domain_add
cargo test -p ployz-e2e https_deploy
```

The `crates/ployz-e2e` binary should continue to avoid false-green behavior
until a real runner exists.

After the implementation lands, run a review focused on design quality:

- Does `DomainReadinessEngine` read as straightforward product orchestration?
- Are claim, operation, certificate, and serving concerns separated cleanly?
- Did Polis make the Ployz code easier to understand, or did Ployz still need
  to know too much about distributed-system mechanics?
- Which awkward edges should become Polis primitives, Ployz product concepts,
  or explicit deferred work?
- Is deploy now simpler because HTTPS readiness is a domain primitive?

---

## Risks

| Risk | Mitigation |
| --- | --- |
| Domain module becomes a hidden cert reconciler | Keep `domain add` foreground and explicit; defer renewal to a separate product primitive. |
| Deploy starts owning ACME again | Deploy depends on a domain readiness port only. |
| DNS preflight sneaks back into this slice | Keep it as a TODO until the DNS probe and ingress ownership model are planned explicitly. |
| Ployz product modules import Polis directly | Keep `scripts/check-boundary.sh` as a required gate. |
| Tests overclaim E2E coverage | Keep scenarios described as in-process acceptance until real daemon/substrate runner exists. |
| Domain readiness hides uncertainty | Preserve certificate, claim, and activation unknown states as distinct failures. |

---

## Completion Criteria

- `DomainReadinessEngine` exists and reads as product orchestration.
- `domain add` semantics are proven with in-process product acceptance tests.
- Deploy HTTPS readiness uses the same domain primitive.
- A post-implementation review is launched to evaluate Ployz code elegance,
  boundary awkwardness, and whether Polis is serving the product code well.
- No domain concepts are added to Polis.
- `just check` and workspace clippy pass.
