---
date: 2026-05-21
topic: polis-ployz-boundary
---

# Polis / Ployz Boundary Requirements

## Summary

Polis is the candidate internal framework namespace that should make Ployz
orchestration code simple. Ployz remains the product: explicit infrastructure
primitives such as deploy, machine membership, volume transfer, routing,
serving, certificates, and environment lifecycle.

Polis should own only the reusable distributed control-plane capabilities that
let those product primitives stay small, visible, and command-shaped. The
boundary must first prove itself in a fresh root workspace. The previous MVP
and root implementations live under `legacy/` as reference material. Separate
`polis` and `ployz` repositories come later, after the boundary is already
clear.

In this document, "Polis" does not imply a public brand, SDK, or repo split. It
is an internal framework boundary until extraction gates are met.

---

## Problem Frame

The current MVP has proven useful product behavior, but the foundational and
product layers are still interwoven. Product code can end up understanding
fact-key taxonomy, projection candidate status, p2panda import behavior, lease
reduction, live request addressing, and authority details before it can express
the actual orchestration rule it owns.

That is the maintenance risk this split addresses. Ployz should read like
orchestration: resolve a deploy, claim ownership, ensure a certificate, start
runtime, publish serving state, drain old runtime, and return visible evidence.
The lower-level mechanics that make those steps safe, durable, and observable
should be Polis capabilities.

The goal is not to support hypothetical backends or turn Ployz into a generic
platform builder. The goal is to create a cleaner internal boundary first. If
the boundary proves itself in the root workspace, a later repo split into
`polis` and `ployz` should be packaging and ownership work rather than
architectural work.

---

## Evidence of Pain

- Product features currently risk depending on framework mechanics such as
  signed fact import/export, projection candidate status, and reducer plumbing.
- ACME ownership and volume transfer both want advisory ownership behavior, but
  duplicated lease replay or reduction glue would make every domain carry its
  own coordination substrate.
- Deploy with HTTPS binding exposes the awkward boundary directly: product code
  must express a deploy rule, but the operation touches authority, signed state,
  projection, ownership, peer calls, certificate material, serving activation,
  and operator-visible failure.
- The risk is not only dependency direction. A product-neutral crate can still
  be semantically shaped around deploy, certificates, or serving if a second
  unlike domain cannot use it cleanly.

---

## Actors

- A1. Ployz feature implementer: Adds product primitives and should work mostly
  in orchestration/domain code.
- A2. Polis framework maintainer: Evolves reusable control-plane capabilities
  without importing Ployz product meaning.
- A3. Operator or agent: Runs explicit Ployz commands and receives visible
  success, failure, and evidence.
- A4. Steady-state role: Applies committed serving/runtime/network state and
  must keep working when the coordinator is unavailable.
- A5. Deploy coordinator: The process currently executing a deploy command. It
  may fail or disappear without making already-committed steady-state serving
  invalid.

---

## Key Flows

- F1. Deploy with HTTPS binding
  - **Trigger:** An operator or agent applies a deploy manifest containing an
    HTTPS binding for a domain.
  - **Actors:** A1, A3, A4, A5
  - **Steps:** Ployz resolves the deploy, claims any required operation
    ownership, ensures a usable certificate exists for the binding, starts or
    updates runtime participants, publishes serving state, and records deploy
    evidence.
  - **Outcome:** The deploy succeeds only if the certificate is usable and the
    serving state is committed. If certificate issuance or activation fails,
    the deploy fails visibly before reporting success.
  - **Covered by:** R1, R2, R9, R10, R11, R12, R13, R20, R21, R23,
    R28

- F2. First boundary proof in the root workspace
  - **Trigger:** The rewrite starts extracting Polis-shaped capabilities from
    product code while using `legacy/` as reference material.
  - **Actors:** A1, A2
  - **Steps:** The first slice separates product-neutral projection substrate
    from Ployz-owned product projection models and reducers, then applies the
    split to the deploy HTTPS path.
  - **Outcome:** Ployz feature code uses typed product-facing ports and does
    not mention raw candidates, fact-log import/export, backend watches, or
    lease reduction outside adapters.
  - **Covered by:** R3, R4, R5, R6, R7, R8, R17, R24, R27, R29

- F3. Second-domain validation
  - **Trigger:** The first proof vertical compiles and works through the new
    boundary.
  - **Actors:** A1, A2
  - **Steps:** A second unlike product domain, such as ACME ownership or volume
    transfer, uses the same Polis capability without adding domain concepts to
    Polis.
  - **Outcome:** The boundary proves semantic reuse, not just product-neutral
    imports.
  - **Covered by:** R18, R19, R30, R31

- F4. Boundary earns repo extraction
  - **Trigger:** Root crates have been organized so framework crates no longer
    import Ployz product crates and at least two domains use the same Polis
    capability cleanly.
  - **Actors:** A1, A2
  - **Steps:** The root workspace proves clean dependency direction, validates product
    behavior through the new boundary, and only then considers moving Polis into
    its own repository.
  - **Outcome:** Repo splitting becomes packaging and ownership work, not a
    redesign.
  - **Covered by:** R27, R28, R30, R31, R32

---

## Requirements

### Boundary and Product Identity

- R1. Ployz must remain the owner of orchestration product meaning: deploy,
  certificates, routes, serving, machines, volumes, environments, runtime
  policy, and operator command semantics.
- R2. Polis must not know Ployz domain workflows such as deploy, ACME issuance,
  route publication, volume transfer, machine removal, or environment promote.
- R3. Polis must provide reusable capabilities only where they remove real
  product-code exposure to distributed control-plane mechanics.
- R4. Polis must remain an internal framework namespace until extraction gates
  are met. Operator-facing CLI, SDK, API, and cloud surfaces should not require
  users to know Polis exists.
- R5. The requirements must not assume a `polis-core`, `polis-messaging`, or
  similar crate taxonomy. Implementation planning may introduce crates after the
  first proof identifies real seams.

### First Boundary Proof

- R6. The first migration phase must prove dependency direction in the fresh
  root workspace before moving code into separate repositories.
- R7. The first proof must split projection into product-neutral substrate and
  Ployz-owned projection modules:
  - Polis owns candidate status, fact source/read APIs, reducer traits,
    cache/snapshot substrate, rebuild mechanics, and watch-source plumbing.
  - Ployz owns product payload enums, ACME/certificate/serving projection
    structs, product reducers, product views, and product interpretation.
- R8. The first proof should extract only capabilities directly exercised by
  deploy with HTTPS certificate ensure. Broad live messaging, operation
  journaling, and lifecycle scaffolding are candidate areas unless this proof
  already needs a narrow version of them.
- R9. If a deploy manifest contains an HTTPS binding, Ployz deploy must ensure
  a usable certificate exists during the deploy and fail the deploy if it cannot
  make the certificate usable.
- R10. A usable certificate means:
  - the certificate chain validates for the exact binding hostname;
  - issuance and activation are authorized for that binding;
  - the private key is present, protected, and never written into logs or
    generic evidence records;
  - the certificate is not expired or revoked at activation time;
  - the certificate has at least the configured minimum remaining lifetime;
  - required serving roles have the needed material; and
  - serving reload or activation has been acknowledged before deploy reports
    success.
- R11. Certificate renewal is not part of deploy. Deploy only ensures initial
  usability for the binding it is applying, but it must fail if the certificate
  would expire before the configured safety window.
- R12. Certificate state must expose expiry, freshness, activation, and
  operator-visible failure status so deferring renewal does not create hidden
  operational debt.
- R13. Ployz serving code must own the meaning of serving state and the rules
  for applying last-good state while the deploy coordinator is unavailable.

### Polis Candidate Capabilities

- R14. Polis should provide identity and authority primitives for scopes,
  principals, grants, membership, writer authority, importer authority, and
  author-key trust when the first proof needs signed state.
- R15. Author keys must be trusted only through explicit membership or grant
  records bound to principals, resources, and scopes. Rotation and revocation
  must be represented, and projections must distinguish historically valid
  facts from facts signed after revocation.
- R16. Every appended or imported fact must be authenticated and authorized
  before it can affect committed projections, snapshots, watches, or lease
  reducers. Rejected facts must be ignored or quarantined with
  operator-visible evidence.
- R17. Polis should provide product-neutral projection substrate primitives, not
  product projection models. Raw candidates and candidate statuses may exist
  below the boundary, but Ployz product code should see typed ports and views.
- R18. Polis should provide advisory coordination primitives for resource
  ownership only when the protected resource has a real fencing point. These
  primitives may include lease facts, TTL, epoch, renewal, release, fencing
  token, deterministic supersession, and local guard ergonomics.
- R19. Every Ployz operation protected by a Polis lease must declare the
  protected resource and the exact mutation boundary where the current fencing
  token is enforced. Stale-token holders must be rejected before mutating
  runtime, serving, volume, or certificate state.
- R20. If the proof needs live peer communication, Polis should extract only the
  narrow addressed request/reply semantics currently needed. Live messages must
  be authenticated, authorized by operation and scope, payload-validated,
  timeout-bounded, and replay-resistant where mutations or lease actions are
  involved.
- R21. If the proof needs operation evidence, Polis may persist generic evidence
  records, idempotency keys, and commit markers, but those records are not
  authoritative unless paired with a Ployz verifier for the domain invariant
  they claim.
- R22. Polis may provide runtime or service-lifecycle scaffolding only where it
  remains product-agnostic: supervision, role ownership, lifecycle health, and
  clean shutdown. Product runtime policy stays in Ployz.

### Ployz Orchestration Responsibilities

- R23. Ployz deploy code must own deploy manifest interpretation, runtime
  participant selection, service revision semantics, route publication
  semantics, drain policy, and deploy failure classification.
- R24. Ployz features must use typed product-facing ports. Product feature code
  must not mention raw projection candidates, fact-log import/export, backend
  watch mechanics, lease reduction, or generic candidate statuses outside
  adapter modules.
- R25. Ployz owns product facts, product views, product reducers, participant
  requests, domain errors, operation phase names, rollback policy, and visible
  command result semantics.
- R26. Steady-state roles may serve last-good committed state only within
  locally verifiable validity bounds. They must refuse or degrade expired,
  revoked, or otherwise unusable certificate/route state and expose freshness
  or health when coordinator failure prevents newer authority checks.

### Migration and Extraction Gates

- R27. Framework crates must not import Ployz product crates. Ployz crates may
  depend downward on Polis crates.
- R28. The first end-to-end validation should be deploy with HTTPS certificate
  ensure because it crosses state, authority, coordination, serving, runtime,
  certificate material, and explicit failure.
- R29. Deploy with HTTPS is the first end-to-end validation, not the first
  extraction target. Smaller slices should prove projection substrate, signed
  state validation, advisory lease fencing, and any needed live request path
  before the full deploy path is judged.
- R30. ACME ownership and volume transfer should become the second-domain
  validation for shared lease/state mechanics after the deploy HTTPS boundary
  proof works.
- R31. A later repo split is allowed only after:
  - at least two unlike Ployz domains use the same Polis capability without
    adding domain-shaped concepts to Polis;
  - Ployz implementers no longer need p2panda, projection candidate, import, or
    lease replay internals for ordinary feature work;
  - framework crates compile without importing deploy, machine, volume, ACME,
    serving, routing, or environment product crates; and
  - no operator-facing API or command surface requires Polis terminology.
- R32. The migration must keep the MVP's existing product guarantee that
  steady-state serving and workloads do not fate-share with the deploy
  coordinator.

### Explicit Non-Goals

- R33. Polis must not become a workflow engine. Domain workflows belong in
  Ployz or in an optional companion layer only after the framework boundary is
  proven.
- R34. Polis must not add strict distributed-lock semantics on top of
  eventually replicated facts. Real exclusivity belongs at the protected
  resource.
- R35. Polis must not be designed around hypothetical backend parity.
  iroh/p2panda is the active path; other adapters are future work only if a real
  need appears.
- R36. Polis must not hide deploy, certificate, serving, or machine policy
  inside background reconcilers. State changes remain explicit Ployz
  operations.

---

## Acceptance Examples

- AE1. **Covers R1, R2, R9, R10, R11, R12, R23.** Given a deploy manifest with
  an HTTPS binding and no usable certificate, when the operator applies the
  deploy, Ployz attempts to obtain or activate the certificate during deploy; if
  issuance, validation, material distribution, activation, or the minimum
  lifetime check fails, the deploy returns a visible failure and does not report
  success.

- AE2. **Covers R10, R11, R12, R26.** Given an otherwise valid certificate that
  expires before the configured safety window, when deploy applies an HTTPS
  binding, deploy fails visibly and certificate state exposes the expiry and
  freshness reason.

- AE3. **Covers R14, R15, R16.** Given a peer imports a well-formed but
  unauthorized signed fact, when projections, snapshots, watches, or lease
  reducers are rebuilt, the fact is rejected before it affects committed state,
  and the rejection leaves operator-visible evidence.

- AE4. **Covers R18, R19, R34.** Given an old lease holder, partitioned lease
  holder, or delayed-renewal holder attempts a protected mutation, when it
  presents a stale fencing token at the protected resource, the mutation is
  rejected before runtime, serving, volume, or certificate state changes.

- AE5. **Covers R21, R25, R33.** Given a deploy operation records generic
  evidence and then crashes before or after a commit marker, when the operation
  is replayed, the marker is treated as non-authoritative unless the Ployz
  verifier confirms the domain invariant and idempotency scope it claims.

- AE6. **Covers R6, R7, R17, R24, R27.** Given the projection boundary is
  extracted in the root workspace, when framework crates compile, they do not import
  ACME, certificate, deploy, serving, routing, machine, volume, or environment
  product modules; Ployz product code sees typed views and ports, not raw
  candidates or candidate statuses.

- AE7. **Covers R13, R26, R32.** Given serving state has been committed, when
  the deploy coordinator is unavailable and a serving-role process restarts, a
  machine restarts, or a local projection cache is corrupted, the serving role
  can restart from locally persisted last-good applied state, enforce local
  validity bounds, and expose freshness or health without requiring coordinator
  liveness.

- AE8. **Covers R30, R31.** Given the deploy HTTPS proof works, when ACME
  ownership and volume transfer both use the same Polis lease/state capability,
  no ACME, certificate, volume, deploy, or serving concepts are added to Polis
  to make the reuse work.

---

## Success Criteria

- Ployz deploy-with-HTTPS code reads as product orchestration, not
  transport/storage/authority choreography.
- Polis capabilities can be identified by dependency direction and semantic
  reuse: product-neutral framework crates below, Ployz product crates above.
- The first proof is narrow enough to implement without inventing a full
  framework, but strong enough to cover certificate usability, signed state,
  projection, lease fencing, and steady-state serving survival.
- ACME ownership and volume transfer validate shared lease/state mechanics only
  after the deploy HTTPS proof works.
- The proof keeps existing MVP guarantees around explicit command failure,
  no hidden reconciliation for mutations, and steady-state serving survival.
- A later `ce-plan` can produce implementation units without inventing the
  product boundary, repo-split timing, lease semantics, cert usability rules, or
  deploy/certificate behavior.

## Root Proof Status

The root workspace now proves the boundary through `crates/polis`,
`crates/ployz`, and `crates/ployz-e2e`:

- `crates/polis` contains only product-neutral identity, authority, records,
  projection substrate, operation evidence, claims, and bounded call receipts.
- `crates/ployz` contains product modules for deploy, ACME, serving, runtime,
  projection, and volume transfer.
- `crates/ployz-e2e` exercises product surfaces for HTTPS deploy, coordinator
  restart after serving checkpoint, ACME ownership, and volume transfer.
- `scripts/check-boundary.sh` rejects Polis imports from Ployz feature modules,
  Ployz imports from Polis, active `legacy/` workspace packages, and raw
  substrate record/projection terms outside adapters.

Extraction is still deferred. The proof shows dependency direction and semantic
reuse, but the crates are still intentionally small and internal.

---

## Scope Boundaries

### Required for the First Proof

- Keep active implementation work in the root workspace.
- Treat `legacy/` as reference material unless a later plan explicitly copies a
  behavior into the new root crates.
- Split projection substrate from Ployz product projections.
- Ensure deploy with HTTPS binding either creates/activates a usable certificate
  and commits serving state or fails visibly.
- Enforce lease fencing only at declared protected-resource mutation points.
- Preserve last-good serving across deploy coordinator failure, process restart,
  machine restart, and local projection cache loss where local validity bounds
  allow it.

### Candidate Framework Areas

- Live peer messaging beyond the narrow addressed request/reply path required
  by the proof.
- Operation journaling beyond generic evidence needed by deploy success/failure
  reporting.
- Runtime/service lifecycle scaffolding beyond product-neutral supervision and
  shutdown.
- A later crate split such as `polis-state`, `polis-projection`, or
  `polis-coordination`, after seams are proven.

### Deferred for Later

- Physically splitting into separate `polis` and `ployz` repositories.
- Public Polis documentation, branding, website, or standalone SDK polish.
- Redlock or other external live-lock adapters.
- NATS or other backend adapters.
- Certificate renewal primitives or maintenance roles.
- A generic workflow engine on top of Polis operation evidence.

### Outside This Product's Identity

- Turning Ployz into a generic orchestration toolkit assembled from knobs.
- Making Polis a general database replication product.
- Using Polis leases as a hidden strict lock or consensus system.
- Hiding deploy, certificate, serving, or machine policy inside background
  reconcilers.

---

## Key Decisions

- Name the reusable framework layer Polis: The name should represent the
  foundation that lets orchestration code be organized, authorized, and
  observable without making Ployz generic.
- Keep Polis internal until it earns extraction: Clean dependency direction and
  semantic reuse in the root workspace are the first meaningful milestones.
  Separate repositories come after the boundary earns it.
- Treat Polis like a framework, not a backend abstraction: The design should
  start from the Ployz code we want to write and provide capabilities that make
  that code simple.
- Keep workflows in Ployz: Polis can provide state, projection, coordination,
  and evidence capabilities, but deploy, certificate, volume, and machine
  workflows remain product-owned.
- Treat leases as a Polis candidate capability with Ployz-enforced fencing:
  Polis may produce lease state and fencing evidence, but the protected resource
  must enforce the token at the mutation boundary.
- Use deploy with HTTPS certificate ensure as the first end-to-end validation:
  It is broad enough to prove the boundary across state, authority,
  coordination, serving, runtime, and explicit failure, but smaller extraction
  slices should come first.

---

## Dependencies / Assumptions

- The active implementation target is the fresh root workspace. `legacy/mvp/`
  and `legacy/crates/` remain reference implementations.
- iroh/p2panda remains the active transport and signed-state direction.
- The existing MVP product constraints still apply: explicit commands, visible
  failures, no hidden reconciliation for mutations, and data-plane continuity
  across deploy coordinator failure.
- Ployz deploy can synchronously ensure certificate usability during deploy
  without making renewal part of deploy.
- Serving roles can persist enough last-good applied state locally to restart
  without deploy coordinator liveness when local validity bounds permit it.
- Operation evidence can be designed as framework evidence without becoming a
  domain workflow engine.

---

## Outstanding Questions

### Deferred to Planning

- [Affects R5, R7, R17][Technical] What is the smallest crate grouping that
  proves Polis without creating too many premature crates?
- [Affects R7, R14, R16][Technical] Should Polis expose product-neutral
  operations as path-like keys, structured operation envelopes, or both during
  migration?
- [Affects R20][Technical] Should any live messaging move into Polis during the
  first proof vertical, or should it remain in Ployz until state-plane
  boundaries are clean?
- [Affects R21, R33][Technical] What is the minimum useful operation evidence
  record that supports deploy reporting without becoming workflow machinery?
- [Affects R28, R32][Technical] Which existing MVP smoke or E2E path should
  become the acceptance proof for deploy with HTTPS certificate ensure?
- [Affects R10, R12, R26][Technical] What minimum remaining lifetime and local
  validity bounds should certificate-backed serving enforce?
