# Polis MVP Extraction Map

This note grounds the Polis/Ployz split in the working `legacy/mvp` code. Polis
primitives should come from these pressures first. A primitive without a row in
this map is speculative until a real Ployz operation needs it.

## Extraction Rule

Polis owns reusable capability mechanics. Ployz owns product meaning and the
final invariant that makes an operation true.

For each primitive:

- Polis should provide a value that proves a narrow capability.
- Ployz should wrap that value in product terms before feature code sees it.
- Product success should depend on verifying the product invariant, not on
  trusting generic evidence.

## Legacy Pressure Map

| Proposed Polis primitive | Legacy MVP pressure | What Polis should carry | What stays in Ployz |
| --- | --- | --- | --- |
| `ClaimGuard<R>` | `legacy/mvp/lease/src/lib.rs` has `LeaseGuard` with resource, holder, epoch, claim hash, expiry, renewal, release, stale guard, foreign guard, and supersession semantics. | Typed resource identity, holder, epoch, fence/claim hash, expiry, renewal/release state, current-guard assertion, and stale/superseded failure classification. | Product resource types, product mutation rules, and the exact protected mutation boundary. |
| Product claim wrapper | `legacy/mvp/acme/src/lib.rs` turns a generic `LeaseGuard` into `AcmeChallengeLease` only after validating the guarded lease resource matches the ACME challenge id. | Generic guard construction and currentness checks. | Wrappers such as `DomainClaim` or `AcmeChallengeLease` that prove the guard matches one product resource. |
| Preflighted record append | `legacy/mvp/volume/src/command.rs` calls `preflight` for the next ownership fact before lease acquisition and side effects, then handles inserted, already-present, and conflict outcomes separately. | Backend-neutral preflight and append outcome types: inserted, duplicate/already-present, and conflict. | Which fact key matters, whether duplicate is idempotent success, and how conflict maps to a product failure. |
| Bounded participant call receipt | `legacy/mvp/volume/src/command.rs` snapshots and receives over participant RPC, then validates reply source, target, transfer id, snapshot id, guid, and byte count before commit. | Timeout-bounded request/reply mechanics and durable call evidence when needed. | Product reply validation and whether the reply proves enough to continue. |
| Post-call invariant check | `legacy/mvp/volume/src/command.rs` rechecks current lease and current volume owner after participant calls and before writing ownership. | A convenient way to re-read a projection or record under a command context. | The domain-specific invariant: current owner unchanged, lease still current, received snapshot matches snapshot request. |
| Open command operation | `legacy/mvp/deploy/src/coordinator.rs` separates decision write, serving commit, projection catch-up, drain, cleanup, and recovery after coordinator death. | Idempotent command ownership, checkpoints, and consuming success/failure closure. | Deploy phases, commit boundary, cleanup status, and product-visible deploy outcome. |
| Projection catch-up proof | `legacy/mvp/deploy/src/coordinator.rs` accepts pending cleanup only after `ProjectionCatchUp` proves the serving commit projected; `legacy/mvp/deploy/src/state_machine.rs` refuses cleanup completion before serving commit. | Freshness metadata and typed projection snapshots. | Which snapshot is sufficient for the product step and what happens when freshness is missing. |
| Key/payload expectation | `legacy/mvp/projection/src/reducer/key_expectation.rs` rejects facts whose typed key and typed payload disagree. | Product-neutral candidate status, authenticated candidate metadata, and reducer input shape. | Product payload enums, key builders, and domain-specific malformed/superseded interpretation. |
| Replay evidence | `legacy/mvp/e2e/src/deploy_restart_recovery_contract.rs` exports/imports facts, serves last-good data plane state during coordinator outage, then recovers pending cleanup from facts. | Durable evidence and replay-safe operation records. | Product verifier that decides whether replayed evidence still proves the operation result. |

## Design Consequences

### Claims Are Not Locks By Themselves

The MVP lease model proves advisory ownership, not exclusive mutation by magic.
Volume transfer still rechecks current lease and current owner immediately
before committing ownership. Polis should make that pattern easy:

```rust
let claim = ctx.claim(volume.resource())?;
let snapshot = participants.snapshot(ctx, &claim, request)?;
let received = participants.receive(ctx, &claim, snapshot)?;
volume_owners.assert_current(ctx, &claim, previous_owner)?;
volume_owners.commit(ctx, &claim, received)?;
```

The claim permits the mutation path. The product owner check decides whether
the mutation is still valid.

### Product Wrappers Are The Ergonomic Boundary

`AcmeChallengeLease` is the good MVP pattern: it hides the raw lease guard and
proves the guard matches an ACME challenge resource. Ployz should use the same
shape for domain readiness:

```rust
let claim: DomainClaim = ctx.claim(DomainResource::for_domain(&domain))?;
let cert = certificates.ensure_usable(ctx, &claim, &domain)?;
let ready = serving.activate(ctx, &claim, cert)?;
```

The feature code should not inspect `ClaimGuard<DomainResource>` or compare
resource strings.

### Evidence Is Not Truth

The MVP deploy restart path recovers from facts but still checks serving commit,
projection catch-up, and cleanup-done shape before reporting a result. Polis
operation evidence should follow the same rule: it may accelerate replay and
explain progress, but product verifiers decide whether a command is complete.

### Projection Substrate Must Stay Below Product Payloads

`payload_matches_key` is reusable as a shape of validation, but the payloads are
product-owned. Polis can carry candidate statuses, authenticated sources,
snapshots, and freshness. Ployz keeps product fact enums, product reducers, and
the rules for malformed, superseded, unauthorized, or conflict candidates.

## Primitive Admission Checklist

Before adding a new Polis primitive, answer:

1. Which `legacy/mvp` operation was harder because this primitive did not
   exist?
2. What fact does holding the value prove?
3. What narrower action does the value permit?
4. Which product invariant is still checked in Ployz?
5. What is the replay story when the process crashes after this value is
   produced?
6. What is the difference between duplicate/idempotent success and conflict?

If those answers are unclear, keep the code in Ployz until a second product
operation needs the same capability.

## Next Slice Implications

- `CommandRunner` must not hide deploy semantics. It should close an operation
  once around a Ployz command result.
- `ClaimGuard<R>` must preserve the MVP lease fields: resource, holder, epoch,
  claim hash or equivalent fence, expiry, renewal/release semantics, and
  currentness checks.
- Domain readiness should use a product wrapper like `DomainClaim`, mirroring
  `AcmeChallengeLease`.
- Replay/idempotency tests should include duplicate writes, conflicts, and
  post-call invariant checks, not only "pending status was recorded."
- The next code slice should start by making the desired Ployz API compile in
  tests, then reshape Polis only where the MVP extraction map justifies it.
