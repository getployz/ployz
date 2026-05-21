---
title: "goal: Make Polis and Ployz Rust-Idiomatic"
type: refactor
status: active
date: 2026-05-21
origin: goal
depends_on:
  - docs/architecture/polis-mvp-extraction-map.md
  - docs/plans/2026-05-21-006-refactor-polis-proof-chain-api-plan.md
---

# goal: Make Polis and Ployz Rust-Idiomatic

## Summary

Drive `crates/polis` and `crates/ployz` to a Rust-idiomatic API shape where
Polis supplies product-neutral capability values and Ployz reads as explicit
product orchestration. Each meaningful slice follows the LFG loop: plan, work,
zero-context API review, fix, test, and only then commit/push/PR when the slice
is coherent.

The goal is not to make the code abstract. The goal is to make Ployz beautiful:
small product operations, typed proof values, ordinary `?` error flow, no
manual evidence bookkeeping in feature modules, and no clean-room primitives
that do not trace back to `legacy/mvp` behavior.

## Current Baseline

Already improved in the current branch:

- `docs/architecture/polis-mvp-extraction-map.md` grounds Polis primitives in
  `legacy/mvp` pressure points.
- Domain readiness has `DomainClaim`, `UsableDomainCertificate`,
  `DomainServingActivation`, and `DomainReady` proof values.
- Domain readiness reads current status and can reuse ready status.
- Deploy runs through `CommandRunner<DeployCommand>` instead of manually
  appending evidence and terminalizing.
- Polis owns operation lifecycle proof values. Backends return raw start/replay
  decisions and Polis mints `OpenOperation` / `OperationReplay`.
- Stored domain readiness is now `DomainReadyRecord`, not the live
  `DomainReady` proof. Reuse upgrades the record through certificate policy and
  serving activation verification.
- Deploy carries `DomainReady` through `DeployOutcome` instead of downgrading
  HTTPS readiness to raw certificate material.
- Ployz operation code is split by lifecycle responsibility under
  `crates/ployz/src/operation/`: command issuing, command running, authority,
  claims, identity, and the Polis boundary are separate modules.
- `crates/ployz/src/operation/` is an explicitly named boundary module that may
  translate to and from Polis types. Ordinary product modules still must not
  import Polis directly.
- Command issuing and running are intentionally separate. Issuing builds a
  command envelope from product identity, fingerprint resources, authority, and
  idempotency. Running owns the operation lifecycle and terminalization.
- Serving commit for HTTPS deploy is now proof-backed. `DomainReady` mints a
  `ServingCommitRequest`; serving commit returns `Result<(), ServingFailure)`;
  activation is checked against a full `ServingActivationCheckpoint`; and
  deploy carries `ServingActivationProof` in the outcome.
- E2E-only proof minting uses an explicit `test-support` feature where external
  fakes need to construct product proof values.
- `CommandRunner` no longer records empty generic observation, checkpoint, or
  failure evidence. Product modules must opt into meaningful checkpoints.
- Command checkpoint byte encoding is centralized in the command boundary.
  Product modules describe checkpoint names and fields; they do not construct
  opaque evidence bytes directly.
- `CommandRunner` can route terminal-success replay through a product verifier
  without rerunning product work or writing another terminal marker. Non-success
  replay remains unavailable.
- Volume transfer replay is no longer a public request mode. Terminal-success
  replay verifies committed ownership and cleanup status through product ports.
- Domain readiness no longer exposes raw `DomainClaimObservation` fields.
  Fresh readiness ports receive a `DomainClaim` product proof, and the service
  is split into reuse and fresh-readiness transitions.
- The unused public `polis::calls` module was removed. Bounded call receipt
  APIs should be reintroduced only when a real Ployz operation needs them.
- Ployz claim guards now wrap Polis claim guards instead of duplicating holder,
  epoch, hash, expiry, and resource proof fields. Domain claim acquisition
  accepts a domain and returns a product `DomainClaim`; the service no longer
  passes raw claim resources through its fresh path.
- Raw claim-proof minting is gated behind explicit `test-support` APIs for E2E
  fakes and crate-local tests. Normal product construction flows through
  acquired guards.
- Unused public Polis projection/record extension traits were removed:
  `ProjectionInput`, `Reducer`, `ProjectionStore`, `ProjectionRead`, and
  `RecordAuthorizer`, along with their unused `RawRecord` / `RecordSource`
  support types. Polis keeps the capability values currently used by Ployz
  adapters instead of exposing speculative framework seams.
- The unused public Ployz projection module and Polis-to-product record adapter
  helper were removed. Product projection APIs should come back from a real
  deploy/domain/volume read path, not from an unused generic module.
- `DomainStatus` no longer implements `Default`; tests and fakes initialize
  `DomainStatus::Unknown` explicitly so uncertainty is visible at construction
  sites.
- Submitted command fences now participate in the Polis request fingerprint
  through a typed `SubmittedFenceFingerprint`. Ployz owns the submitted fence
  token; Polis owns only the product-neutral idempotency comparison value.
- `CommandRunner` preserves the original product failure when best-effort
  failed-operation terminalization fails. Successful product work still
  requires a durable success marker.
- Deploy and volume transfer now have product-owned command issue helpers that
  derive command kind, payload hash, resources, and volume fence participation
  from typed product requests. Generic `MutationIntent`, `CommandKind`, and
  `FingerprintedResource` are no longer re-exported as normal product API.
- Deploy terminal-success replay now verifies the existing domain ready record,
  runtime participant, and serving activation without rerunning mutating deploy
  work.
- Deploy and volume engines now accept issued product command tokens that own
  the request they fingerprinted, so callers cannot issue a command for one
  product request and execute another under the same operation fingerprint.
- Domain adapter traits are now implementable by real external adapters:
  `DomainReadyRecord` has a validating public constructor,
  `DomainServingActivation` can be minted from an active serving generation,
  and `DomainClaim` exposes its product-safe guard/submitted fence capability.
- Deploy command fingerprinting now includes the behavior-affecting
  `DeployRequest` deadline because runtime activation uses that deadline during
  product work.
- A zero-context full API roast after the issued-command slice confirmed that
  command issuing and command running are split in the right direction, while
  flagging domain adapter constructibility, deploy deadline identity, and
  non-success replay/outcome policy as the remaining blockers.
- `just check` and clippy passed after those slices.

Still weak:

- Failed/interrupted deploy retry after partial mutation remains the main
  unresolved API issue. Non-success replay still returns `ReplayUnavailable`;
  changing that needs an explicit product outcome policy for failed,
  interrupted, and open operation states rather than a small deploy patch.
- The low-level operation boundary still exposes `CommandEnvelope` because
  `CommandBackend` and `CommandRunner` are public operation primitives. Product
  deploy and volume APIs no longer expose it directly.
- A refreshed fence for the same logical command now requires a new
  idempotency key. That is safer than silently treating two fenced attempts as
  the same request, but product command issuers still need to make that policy
  obvious at their API boundary.
- Failed-operation terminalization remains best-effort. If recording the
  failed marker fails, the operator still sees the product failure, but the
  operation may remain open until the broader replay/outcome slice gives that
  lifecycle failure a better status surface.
- There is not yet a real claim backend/acquisition adapter. The current code
  has the intended proof shape, but production claim acquisition still needs a
  concrete adapter that returns Polis guards instead of test-support minting.
- Polis still exports public `records` and `projections` capability modules
  that are not consumed by the current Ployz product surface. They are grounded
  in the legacy extraction map, but remain at risk of reading as speculative
  framework nouns until a real read/append path consumes them or they are
  trimmed.

## Requirements

- R1. Ployz product modules must not import `polis` directly. Only adapters,
  composition, and explicitly named boundary modules may import Polis. In the
  current shape, `crates/ployz/src/operation/` is the named operation boundary.
- R2. Ployz feature modules must not call `record_evidence`, `terminalize`, or
  raw operation-store APIs.
- R3. Ployz product code should use product proof values: `DomainClaim`,
  `UsableDomainCertificate`, `DomainReady`, volume ownership receipts, deploy
  serving commit proofs, and similar values.
- R3a. Capability/proof values must not be publicly forgeable. Constructors
  that mint authority, operation, claim, or readiness proofs must be private,
  crate-visible behind adapters, or gated behind explicit test-support APIs.
- R4. Polis public primitives must map to `legacy/mvp` pressure points from
  `docs/architecture/polis-mvp-extraction-map.md`.
- R5. Polis must own generic capability mechanics: operation lifecycle,
  idempotency/fingerprint conflict, claim guard fields, bounded call receipts,
  projection snapshot freshness, and record append outcomes.
- R6. Ployz must own product meaning: deploy phases, domain status, certificate
  policy, volume owner invariants, runtime/serving semantics, and operator
  failures.
- R7. Product APIs should support ordinary Rust `?` flow. Failure recording is
  centralized at the command boundary or status boundary, not repeated inside
  every failure branch.
- R8. Replay/idempotency must be tested with real second attempts and product
  invariant verification, not just evidence/status presence.
- R9. Each meaningful API slice must be reviewed by a zero-context subagent
  that is allowed to roast the design. Actionable review findings must be fixed
  or recorded as residual work before the slice is considered complete.
- R10. Each slice must pass `just check` and
  `cargo clippy --workspace --all-targets -- -D warnings`.

## LFG Slice Policy

For this goal, one "slice" means a coherent API movement that can be reviewed
on its own. Each slice follows:

1. Maintain or create the slice plan in `docs/plans/`.
2. Implement only that slice.
3. Run focused tests.
4. Spawn a zero-context API reviewer with no forked chat context.
5. Fix accepted findings.
6. Run `just check` and clippy.
7. Commit/push/PR when the branch reaches a coherent checkpoint.

Zero-context review prompt requirements:

- Tell the reviewer to read `VISION.md`, `AGENTS.md`, the active plan, and the
  touched files.
- Tell the reviewer to focus on API shape, Rust idiom, boundary leaks, MVP
  grounding, replay/idempotency, and product readability.
- Do not give the reviewer the conversation history.
- Require file/line findings and concrete redesign recommendations.

## Implementation Slices

### S0. Make Proof Values Non-Forgeable

**Goal:** Align the type system with the docs: holding a proof value should
mean it came from the boundary that can prove it.

**Modify:**

- `crates/polis/src/authority.rs`
- `crates/polis/src/operations.rs`
- `crates/polis/src/claims.rs`
- `crates/ployz/src/domain/mod.rs`
- test/e2e support helpers that currently mint proofs directly

**Work:**

- Hide or restrict constructors and fields for `Authorized`, `OpenOperation`,
  `ClaimLease`, `ClaimGuard`, and `DomainClaim`.
- Add explicit test-support constructors only where external e2e fakes need to
  produce capability values.
- Keep product value validation constructors public only when they do not mint
  authority. For example, parsing `DomainName` is fine; minting `DomainClaim`
  is not.
- Update tests and e2e fakes to use named test support helpers so fake proof
  creation is visible.

**Tests:**

- Product tests cannot construct claim/operation proofs through ordinary public
  struct literals.
- E2E fakes use clearly named test-support constructors.
- Domain mismatched resource rejection is still covered.

**Completion Gate:**

- The zero-context reviewer should no longer be able to say proof values are
  trivially forgeable through public fields or ordinary public constructors.

### S1. Polis-Backed Operation Boundary

**Status:** Completed for the current MVP slice except product replay
verification, which is deferred to S5.

**Goal:** Stop duplicating operation lifecycle concepts in Ployz. Make Ployz
`CommandRunner` a product facade over Polis operation lifecycle values.

**Modify:**

- `crates/polis/src/operations.rs`
- `crates/ployz/src/operation/`
- `crates/ployz/src/adapters/polis.rs`
- e2e fakes that implement operation persistence

**Work:**

- Introduce a Ployz operation backend trait that starts or replays commands in
  product terms but is implemented by a Polis adapter.
- Remove duplicated Ployz-owned operation lifecycle types when Polis already
  owns the concept.
- Keep product-facing `CommandContext<C>` in Ployz, but make its lifecycle
  state come from Polis `OpenOperation<C>`.
- Make `CommandRunner` own request fingerprinting, `start_or_replay`, replay
  handling, and consuming operation closure.
- Replays currently return `ReplayUnavailable` without appending a second
  terminal marker. Returning a previous closed command or invoking a product
  verifier is deferred to S5.
- Preserve Ployz error mapping and product evidence encoding at the adapter
  boundary.

**Tests:**

- Same idempotency and fingerprint replays return `ReplayUnavailable` for now
  without rerunning product work or writing a second terminal marker.
- Same idempotency with different fingerprint conflicts.
- Success/failure closure consumes the open command.
- Product modules cannot append evidence after closure.
- Deploy tests still assert one terminal result per command.

**Completion Gate:**

- `crates/ployz/src/operation/` has no duplicate operation state machine that
  competes with `crates/polis/src/operations.rs`.
- Product modules still do not import `polis`.

### S2. Domain Ready Reuse, Deploy Proof Carrying, And Serving Activation

**Status:** Completed for the current MVP slice.

**Goal:** Stop treating stored domain ready status as a live proof, and make
deploy carry the HTTPS readiness and serving activation proofs it relies on.

**Modify:**

- `crates/ployz/src/domain/mod.rs`
- `crates/ployz/src/deploy/mod.rs`
- `crates/ployz/src/serving/mod.rs`
- `crates/ployz-e2e/src/scenarios/domain_add.rs`
- `crates/ployz-e2e/src/scenarios/https_deploy.rs`
- `crates/ployz-e2e/src/scenarios/coordinator_restart.rs`
- `crates/ployz/Cargo.toml`
- `crates/ployz-e2e/Cargo.toml`

**Work:**

- Split stored `DomainStatus::Ready` or `DomainReadyRecord` from live
  `DomainReady` proof if needed.
- On reuse, verify serving activation/projection freshness before returning
  `DomainReady`, or return a stored record that must be upgraded by a verifier.
- Make deploy keep `DomainReady` or a narrower `HttpsReadinessProof` through
  serving commit and `DeployOutcome`.
- Make serving commit proof-backed: callers cannot build a public raw serving
  snapshot for HTTPS deploy; `DomainReady` produces a `ServingCommitRequest`
  and activation verification mints `ServingActivationProof`.
- For deploy serving commit, make activation verification compare the complete
  route, hostname, target, and generation identity, not generation alone.
- Domain readiness reuse still persists only domain, certificate, and serving
  generation, then revalidates through `DomainServingPort`; carrying full route
  activation identity through `DomainReadyRecord` is not part of the current
  MVP slice.
- Keep proof-minting constructors crate-visible or behind explicit test-support
  APIs.
- Preserve typed domain failure detail where deploy can use it; do not flatten
  all certificate/readiness failures into one generic variant if the caller can
  branch on them.

**Tests:**

- Stored ready with usable certificate but stale/missing serving activation is
  not reused as `DomainReady`.
- Deploy outcome carries the domain/HTTPS readiness proof.
- Serving commit consumes or references the readiness proof for the hostname it
  publishes.
- Wrong-route or wrong-host activation observation does not become deploy
  success.
- Serving activation observation returns a proof value on success, not a
  boolean that product code may ignore.
- Retry after checkpoint verifies readiness before success.

**Completion Gate:**

- The happy path reads as HTTPS readiness proof -> runtime activation -> serving
  commit, not certificate preflight plus unrelated route write.
- A zero-context reviewer can no longer block the slice on public serving
  snapshot construction, public commit receipt construction, target-only
  activation lookup, or boolean activation proof checks.

### S3. Polis-Backed Claims With Product Wrappers

**Status:** Completed for the current MVP slice. Polis claim/fence values carry
`ClaimHash`, reject reserved epochs, and mint typed guards. Ployz
`ClaimGuard<R>` now wraps a Polis guard instead of mirroring proof fields, and
`DomainClaim` is built from an acquired guard. Full lease backend,
renewal/release semantics, and submitted-fence validation beyond the current
request envelope remain deferred.

**Goal:** Make claim guards preserve MVP lease realities and flow through
product wrappers.

**Modify:**

- `crates/polis/src/claims.rs`
- `crates/ployz/src/operation/`
- `crates/ployz/src/domain/mod.rs`
- `crates/ployz/src/acme/mod.rs`
- `crates/ployz-e2e/src/scenarios/acme_ownership.rs`
- `crates/ployz-e2e/src/scenarios/domain_add.rs`

**Work:**

- Add `ClaimHash` to Polis claim values so same-epoch supersession can be
  represented. Legacy lease correctness depends on this.
- Make epoch construction reject zero/overflow-reserved values if the legacy
  lease model requires it.
- Ensure Polis claim guards carry resource, holder, epoch, claim hash/fence,
  expiry, renewal/release semantics, and currentness checks.
- Remove Ployz duplicate claim primitives where Polis owns the same proof.
- Keep `DomainClaim` and ACME/domain product wrappers as the Ployz-facing API.
- Change domain claim acquisition so the port returns a product `DomainClaim`
  instead of raw fields assembled by `DomainReadinessService`.
- Extend or replace `SubmittedFenceToken` so ACME ownership is not granted by
  mere presence of `{ resource, holder, epoch }`; it must carry the claim hash
  or a Polis-backed fence token.
- Ensure stale/superseded/foreign/mismatched guards map into structured Ployz
  failures.

**Tests:**

- Mismatched resource fails before certificate/serving mutation.
- Stale guard fails before later mutation.
- Expired guard is not current.
- Product wrappers hide raw generic guards from feature code.

**Completion Gate:**

- Domain and ACME code accept product claim wrappers, not raw claim records.
- Claim semantics trace to `legacy/mvp/lease/src/lib.rs` and
  `legacy/mvp/acme/src/lib.rs`.

### S4. Volume Onto Command Context

**Status:** Completed before this checkpoint. The verification remains in the
suite because this slice protects important MVP volume semantics.

**Goal:** Bring volume transfer onto the same command boundary so it stops
manually handling operation evidence.

**Modify:**

- `crates/ployz/src/volume/mod.rs`
- `crates/ployz/src/operation/`
- `crates/ployz-e2e/src/scenarios/volume_transfer.rs`

**Work:**

- Replace direct `OperationPort` use in volume with `CommandRunner` or a
  `CommandContext<VolumeTransferCommand>`.
- Preserve the MVP pattern: preflight, claim, snapshot, receive, recheck lease,
  recheck current owner, commit ownership, verify committed owner.
- Introduce product proof values for snapshot receipt, receive receipt, and
  committed ownership only where they prevent real mistakes.

**Tests:**

- Stale claim rejects before source mutation and before later mutation.
- Forged receive does not commit ownership.
- Terminal-success replay verifies committed ownership without source mutation.
- Cleanup failure remains visible without rewriting ownership.
- No `record_evidence` or `terminalize` calls remain in volume product code.

**Completion Gate:**

- Volume reads as a product operation, not a framework transaction.
- Volume still preserves the post-call invariant checks from `legacy/mvp`.

### S5. Command Summary, Evidence Encoding, And Replay Boundary

**Status:** In progress. Narrow slices have removed generic empty evidence,
moved checkpoint byte encoding behind the Ployz command boundary, added
success-only replay verification for volume, made submitted fences part of the
operation fingerprint, and stopped failed-marker terminalization from masking
the original product failure. Deploy and volume command issuance now derives
replay metadata from typed product requests, and deploy terminal-success replay
now verifies product state without mutation. Product engines accept issued
command tokens that carry their fingerprinted request. Failed/interrupted
deploy retry after partial failure remains open.

**Goal:** Product modules return typed summaries/proofs; the command boundary
encodes safe evidence into Polis records and owns replay behavior.

**Modify:**

- `crates/ployz/src/operation/`
- `crates/ployz/src/deploy/mod.rs`
- `crates/ployz/src/domain/mod.rs`
- `crates/ployz/src/volume/mod.rs`
- `crates/ployz/src/adapters/polis.rs`
- `crates/polis/src/operations.rs`

**Work:**

- Define typed command summaries where needed.
- Keep private key material and unsafe payloads out of generic evidence.
- Keep exact checkpoint byte-format tests inside the command boundary; product
  E2E tests should assert product outcomes and checkpoint occurrence/order, not
  private encoding bytes.
- Make operation replay consult product verifiers before returning success.
  Product verifiers are invoked only for terminal-success replay; pending,
  failed, and interrupted replay cannot become success.
- Keep evidence writes at the command boundary; do not reintroduce manual
  evidence bookkeeping into deploy, domain, or volume product code.

**Tests:**

- Evidence rendering cannot include private key material.
- Product modules do not construct raw `Vec<u8>` checkpoint payloads.
- Duplicate evidence is idempotent where product verifier confirms success.
- Non-terminal or failed replay does not call product verifiers.
- Conflict evidence does not become success.

**Completion Gate:**

- Product modules do not build opaque bytes or generic evidence records
  directly.
- Operation evidence is useful but not treated as product truth.

### S6. Typed Receipt Store Boundary

**Status:** Deferred. The unused speculative `polis::calls` API was deleted in
the API roast pass. Reintroduce this slice only when a real Ployz operation
needs bounded call receipts.

**Goal:** Keep mutation receipts typed through the store seam instead of
defaulting back to bytes and generic `polis::Error`.

**Modify:**

- future `crates/polis/src/calls.rs` replacement, only when a product caller
  exists
- Ployz adapters or fakes that store call receipts

**Work:**

- Start from the product caller and add only the receipt shape it needs.
- Make the receipt store generic or give it associated success/failure types.
- Keep serialization below the adapter boundary.
- Add tests that persist and replay typed receipts, not only construct them in
  memory.

**Tests:**

- Typed success receipt stores and replays without erasing type.
- Typed failure receipt stores and replays without erasing type.
- Same idempotency with different payload conflicts.

**Completion Gate:**

- `MutationReceipt<T, E>` is not just a typed value; the persistence seam can
  preserve the typed contract through an adapter.

### S7. Projection/Record Append Outcome Shape

**Status:** Simplification slice completed for unused public Polis surfaces.
Unused public Ployz projection wrappers were also removed. Append/read outcomes
remain deferred until a real projection or append store exists in the root
rewrite.

**Goal:** Model append outcomes and projection snapshots in a way that supports
MVP duplicate/conflict/freshness behavior.

**Modify:**

- `crates/polis/src/records.rs`
- `crates/polis/src/projections.rs`
- future Ployz product projection module, only when a read path needs it
- product adapters that append or read records

**Work:**

- Introduce clear inserted/already-present/conflict outcomes where missing.
- Keep product payload enums and reducers in Ployz.
- Ensure projection freshness is explicit at the Ployz boundary.
- Do not publish generic reducer/store traits before a production adapter needs
  them.
- Do not publish Ployz projection ports before a product read path needs them.

**Tests:**

- Duplicate append is distinguishable from conflict.
- Unknown freshness is not fresh.
- Product code branches on typed outcomes, not display strings.

**Completion Gate:**

- This slice can explain how it maps to
  `legacy/mvp/projection/src/reducer/key_expectation.rs` and volume write
  outcomes.

### S8. API Roast And Simplification Pass

**Goal:** Remove abstractions that survived earlier slices but do not prove
real invariants.

**Modify:**

- Any touched crate API from prior slices.
- `README.md`
- `docs/architecture.md`

**Work:**

- Run a zero-context review over the complete Polis/Ployz API.
- Remove dead wrapper types, redundant ports, stringly identities, and
  framework nouns that are not capability values.
- Re-check every public type against the proof test.

**Tests:**

- Full workspace tests.
- Boundary checks.
- Clippy with warnings denied.

**Completion Gate:**

- The reviewer agrees the API shape is coherent or any remaining disagreement
  is captured as durable residual work.
- `CommandRunner`, claims, domain readiness, deploy, and volume all read as
  Rust product code rather than hand-written transaction scripts.

## Verification

Every slice must finish with:

```sh
just check
cargo clippy --workspace --all-targets -- -D warnings
```

Boundary checks:

```sh
rg "use polis|polis::" crates/ployz/src --glob '!crates/ployz/src/adapters/**' --glob '!crates/ployz/src/composition.rs' --glob '!crates/ployz/src/operation/**'
rg "record_evidence|terminalize" crates/ployz/src/domain crates/ployz/src/deploy crates/ployz/src/volume
rg "claim\\..*==|==.*claim\\." crates/ployz/src/domain
```

## Done

The goal is complete when:

- Polis public APIs are capability values grounded in `legacy/mvp`.
- Ployz product modules do not manually manage generic evidence,
  terminalization, or raw Polis primitives.
- Deploy, domain readiness, and volume transfer are command-shaped and readable.
- Replay/idempotency behavior is tested with real second attempts and product
  verification.
- Zero-context API review has no unresolved design-blocking findings.
- The branch is committed, pushed, and has a PR with green checks or durable
  residual records.
