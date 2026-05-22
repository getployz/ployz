---
title: "feat: Add Polis Fact And Projection Substrate"
type: feature
status: active
date: 2026-05-22
origin: docs/brainstorms/2026-05-21-polis-ployz-boundary-requirements.md
depends_on:
  - VISION.md
  - docs/brainstorms/2026-05-21-polis-ployz-boundary-requirements.md
  - docs/plans/2026-05-22-002-refactor-polis-observable-operations-plan.md
  - legacy/mvp/architecture.md
  - legacy/mvp/slice-005-fact-projection-plan.md
  - legacy/mvp/slice-018b-p2panda-fact-substrate.md
  - legacy/mvp/slice-019b-persistent-p2panda-fact-store-plan.md
  - legacy/mvp/slice-048-product-membership-foundation-plan.md
  - legacy/mvp/slice-054-product-deploy-command-plan.md
---

# feat: Add Polis Fact And Projection Substrate

## Problem Frame

The observable-operation refactor moved deploy, volume transfer, machine add,
and ACME issuance away from generic workflow orchestration. Product code now
reads mostly as observe, diff, apply. The next missing substrate is durable
cluster memory.

Today the root crates express durable state through product-specific ports and
test fakes: machine membership observes `MachineStatus`, domain readiness
records `DomainStatus`, serving commits check activation, and volume transfer
observes ownership and cleanup state. That is the right product shape, but the
storage and projection mechanics below those ports are still not a reusable
Polis capability.

The MVP already proved the important split:

- durable signed facts are cluster truth;
- derived indexes, SQLite projections, and serving snapshots are rebuildable
  views;
- notifications are hints, not correctness;
- candidate status, authorization, conflicts, and watermarks must be explicit;
- product reducers decide product meaning.

This plan adds the first root-workspace version of that split without pulling
in p2panda, iroh-docs, NATS, or any network sync decision. Polis should expose
a small product-neutral fact/projection substrate. Ployz should own product
fact families, reducers, typed views, command semantics, and live verification.

## Requirements Trace

- From `VISION.md`: the daemon is disposable, the data plane outlives the
  control plane, operations are foreground commands, durable state records
  operator intent and lifecycle events, and live state must not be inferred
  into stored truth.
- From `docs/brainstorms/2026-05-21-polis-ployz-boundary-requirements.md`:
  Polis owns candidate status, fact source/read APIs, reducer traits,
  cache/snapshot substrate, rebuild mechanics, and watch-source plumbing;
  Ployz owns product payload enums, reducers, views, and interpretation.
- From `docs/plans/2026-05-22-002-refactor-polis-observable-operations-plan.md`:
  Sub-Slice G asks for a fact/projection extraction plan rather than expanding
  `Attempt` into observable operations.
- From `legacy/mvp/slice-005-fact-projection-plan.md`: projections are
  rebuildable from facts; snapshots are data-plane input; dropped
  notifications must not affect correctness.
- From `legacy/mvp/slice-018b-p2panda-fact-substrate.md`: signed envelope,
  payload hash binding, append-log validation, and local operation persistence
  live below the projection seam; business reducers do not change.
- From `legacy/mvp/slice-019b-persistent-p2panda-fact-store-plan.md`: the
  operation log is durable truth; derived Ployz indexes and projection SQLite
  are disposable; persistent role startup rebuilds indexes from operations.
- From the current root code: `crates/ployz/src/machine.rs`,
  `crates/ployz/src/deploy/mod.rs`, `crates/ployz/src/domain/mod.rs`,
  `crates/ployz/src/serving/mod.rs`, and `crates/ployz/src/volume/mod.rs`
  already expose product-facing observe/status/record ports that should become
  adapters over a shared substrate.

## Scope

In scope:

- Product-neutral Polis fact identity, append, candidate, payload-read, cursor,
  and projection freshness types.
- An in-memory Polis fact store/projection source for API proof and tests.
- Product-owned Ployz fact families and reducers for at least two unlike
  product domains.
- Machine membership as a preparatory consumer because it is small, durable,
  and projection-shaped.
- Deploy-serving/domain status as the first proof gate because HTTPS deploy
  crosses state, authority, certificate material, serving, runtime, projection
  catch-up, and live verification.
- Volume ownership and cleanup as the second unlike validation after HTTPS
  deploy proves the boundary under real product stress.
- Boundary tests proving raw fact candidates and projection statuses stay out
  of ordinary Ployz product modules.

Out of scope:

- Choosing or integrating a production network backend.
- p2panda, iroh-docs, NATS JetStream/KV, or SQLite persistence.
- Background reconcilers or autonomous projection loops.
- Generic workflow, operation phase, or attempt expansion.
- Product fact schemas inside Polis.
- Serving reload, ACME protocol, runtime activation, ZFS, or gateway side
  effects beyond the existing product ports.

## Key Decisions

### D1. Facts Are Product-Neutral Durable Records

Polis facts should carry product-neutral identity and bytes: scope, author,
resource, key, kind/schema, payload digest, cursor, authority epoch, and
optional submitted fence fingerprint. Polis may validate append idempotency,
payload binding, conflicts, and product-neutral grants. It must not interpret a
key as "machine joined", "route active", "volume owner", or "certificate
ready".

Scope authorization is not enough for fact writes. Fact append authorization
must also prove that the principal may write the requested resource/key/kind,
and later backend plans must separately model author-key trust and replica
import authority. A replica importer must never become a writer by virtue of
being allowed to import remote operations.

### D2. Ployz Owns Fact Families And Reducers

Ployz modules define product fact payloads and reducers. For example, machine
membership can reduce joined/removing/tombstoned facts into `MachineStatus`;
volume can reduce ownership and cleanup facts into `OwnershipObservation` and
`CleanupStatus`; serving/domain can reduce committed route/domain facts into
typed views. Polis only feeds authorized candidates and payload bytes.

### D3. Candidate Status Is Not Product Status

`CandidateStatus` belongs below the boundary. It describes whether a durable
candidate can participate in projection: verified, conflicting, unauthorized,
unverified, cross-scope, missing payload, or substrate-malformed. Product
payload decode failures are not Polis candidate status, because Polis cannot
understand product schemas. Product reducers/adapters must surface product
decode failures as typed product projection errors or rejected product facts.
Product code should see `MachineStatus`, `DomainStatus`,
`OwnershipObservation`, and serving/domain views, not raw candidate labels.

### D4. Projection Catch-Up Is A Proof Of Visibility, Not Success

Polis can prove "this projection has consumed facts through cursor X." Ployz
must still inspect the resulting typed view and live observations before
declaring success. This mirrors the ACME Attempt proof: a completed external
attempt is not a usable certificate until Ployz re-observes certificate
usability.

For write-then-verify flows, catch-up is only the first half of the proof. The
product adapter must re-read the typed projection at or beyond the fact receipt
cursor and verify the exact product identity it just committed. For HTTPS
deploy, that means route, hostname, target, and generation must match before
live serving activation can satisfy success.

### D5. Notifications Are Hints

Any watch or notification API introduced later must be optional acceleration.
Correctness comes from full candidate listing, payload reads, reducer rebuild,
and explicit catch-up to a cursor. A missed notification cannot make a command
incorrect.

### D6. Backend Choice Is Deferred

The root plan starts with an in-memory Polis store and projection source. A
persistent p2panda/iroh/NATS adapter should follow only after the API proves
itself against multiple Ployz domains. This avoids copying the MVP's backend
weight before the boundary is settled.

The in-memory proof is not a durability proof. It must still model the API
invariants that persistent adapters will need: immutable receipts, replayable
idempotency, stable cursors, rebuild from facts, candidate conflicts, payload
identity binding, redacted rejection health, and catch-up semantics. Reopen,
network sync, and disk corruption behavior belong in a later backend plan.

### D7. Live Observation Stays Outside Projection

Runtime participant status, gateway activation acknowledgement, ACME directory
state, and source/target volume side effects remain live/product observations.
Facts may record committed intent or lifecycle events; they do not replace the
live verification a command needs at decision time.

## Target API Shape

This is directional API guidance, not implementation spec.

```rust
pub trait FactStore {
    fn append(&self, request: FactAppendRequest) -> Result<FactAppendOutcome>;
    fn list_candidates(&self, query: FactQuery) -> Result<Vec<FactCandidate>>;
    fn read_payloads(&self, candidates: &[FactCandidate]) -> Result<FactPayloadBatch>;
}

pub struct FactAppendRequest {
    pub operation: OperationId,
    pub idempotency: IdempotencyKey,
    pub authority: Authorized<FactAppendScope>,
    pub grant: FactWriteGrant,
    pub resource: ResourceId,
    pub key: FactKey,
    pub kind: FactKind,
    pub payload: FactPayload,
    pub submitted_fence: Option<SubmittedFenceFingerprint>,
}

pub enum FactAppendOutcome {
    Appended(FactReceipt),
    Replayed(FactReceipt),
    Conflict(FactConflict),
    Rejected(FactRejection),
}
```

```rust
pub trait ProjectionSource {
    fn project<R: FactReducer>(&self, request: ProjectionRequest<R>) -> Result<ProjectionSnapshot<R::View>>;
    fn catch_up(&self, view: ProjectionView, cursor: FactCursor, deadline: SystemTime)
        -> Result<ProjectionCatchUp>;
}

pub trait FactReducer {
    type View;
    type Error;

    fn reduce(&self, candidates: Vec<VerifiedFact>) -> Result<Self::View, Self::Error>;
}

pub struct ProjectionSnapshot<T> {
    pub view: T,
    pub source_cursor: Option<FactCursor>,
    pub freshness: ProjectionFreshness,
    pub health: ProjectionHealth,
}
```

The important constraints:

- `FactReducer` lives in or is implemented by Ployz product modules.
- `VerifiedFact` is still product-neutral bytes plus identity/cursor metadata.
- Product modules should receive typed views from adapters, not operate on raw
  `FactCandidate` or `CandidateStatus`.
- `FactWriteGrant` is product-neutral but resource/key/kind-aware. It proves
  append permission for the concrete fact being written, not just broad scope
  membership.
- `ProjectionHealth` exposes redacted reason counts and last projection errors
  without leaking rejected payloads or private candidate metadata.

## Implementation Units

### Unit 1. Polis Fact Core

Files:

- `crates/polis/src/facts.rs`
- `crates/polis/src/lib.rs`
- `crates/polis/src/error.rs`

Work:

- Add `FactKey`, `FactKind`, `FactId`, `FactCursor`, `FactPayloadDigest`,
  `FactPayload`, `FactReceipt`, `FactAppendRequest`, and
  `FactAppendOutcome`.
- Add `FactWriteGrant` and fact-write authorization checks for resource, key,
  kind, principal, scope, and authority epoch. This is separate from broad
  scope authorization.
- Add candidate status types that distinguish verified, conflict,
  unauthorized, unverified, missing payload, substrate-malformed payload, and
  cross-scope candidates.
- Add an in-memory `FactStore` implementation for tests and root proof work.
- Make append idempotency fingerprinted by operation, idempotency key,
  authority epoch, resource, key, kind, payload digest, and submitted fence
  fingerprint.
- Preserve all same-key/different-payload conflicts as candidates rather than
  overwriting.

Tests:

- `crates/polis/src/facts.rs`: appending the same request replays the same
  receipt.
- `crates/polis/src/facts.rs`: same key and different payload returns
  conflict and leaves both candidates visible.
- `crates/polis/src/facts.rs`: same idempotency key with different fingerprint
  returns conflict.
- `crates/polis/src/facts.rs`: payload reads are bound to exact candidate
  identity, not just content hash.
- `crates/polis/src/facts.rs`: submitted fence fingerprint participates in
  append fingerprinting.
- `crates/polis/src/facts.rs`: broad scope authorization without a matching
  fact write grant cannot append.
- `crates/polis/src/facts.rs`: wrong resource, key, kind, principal, or
  authority epoch in the write grant rejects append.
- `crates/polis/src/facts.rs`: replica import authority does not authorize
  local fact writes.

### Unit 2. Polis Projection Source

Files:

- `crates/polis/src/projection.rs`
- `crates/polis/src/lib.rs`
- `crates/polis/src/error.rs`

Work:

- Add `ProjectionView`, `ProjectionKey`, `ProjectionRequest`,
  `ProjectionSnapshot`, `ProjectionFreshness`, `ProjectionHealth`, and
  `ProjectionCatchUp`.
- Add a reducer-facing source API that consumes verified fact candidates and
  payloads without exposing backend watches or storage internals.
- Add explicit source cursor/watermark tracking.
- Add `catch_up(view, cursor, deadline)` with structured outcomes:
  caught up, timeout, freshness unknown, or projection failed.
- Keep projection rebuilding synchronous and explicit for the first proof.
- Keep substrate malformed payload failures distinct from product payload
  decode failures reported by product reducers.

Tests:

- `crates/polis/src/projection.rs`: full rebuild after clearing a projection
  cache yields the same view.
- `crates/polis/src/projection.rs`: missing payload prevents a verified
  projection view and reports structured projection failure.
- `crates/polis/src/projection.rs`: catch-up succeeds only when source cursor
  is at or beyond the requested cursor.
- `crates/polis/src/projection.rs`: stale or unknown freshness is explicit and
  does not return a fresh snapshot.
- `crates/polis/src/projection.rs`: projection health reports redacted counts
  for rejected candidates without exposing rejected payload bytes.
- `crates/polis/src/projection.rs`: product reducer decode failure is surfaced
  as reducer error, not as Polis candidate status.

### Unit 3. Ployz Fact Adapter Boundary

Files:

- `crates/ployz/src/facts/mod.rs`
- `crates/ployz/src/adapters/polis.rs`
- `crates/ployz/src/lib.rs`
- `scripts/check-boundary.sh`

Work:

- Add Ployz-facing adapter helpers that hide raw Polis candidates from feature
  modules.
- Add product-owned traits for appending product facts and reading product
  views.
- Update the boundary script so raw substrate types are allowed only in
  `crates/ployz/src/adapters/**` and `crates/ployz/src/facts/**`. The denylist
  should cover `FactCandidate`, `CandidateStatus`, `VerifiedFact`,
  `FactReducer`, `ProjectionSource`, `ProjectionSnapshot`,
  `FactAppendOutcome`, `FactReceipt`, `ProjectionCatchUp`, backend fact store
  types, and future raw aliases added in these modules.
- Add compile-fail or boundary-fixture tests that catch type aliases and
  re-exports, not only direct imports.
- Keep product modules using typed ports such as `MachineMembershipPort`,
  `DomainStatusPort`, `ServingPort`, and `VolumeOwnershipPort`.

Tests:

- `scripts/check-boundary.sh`: direct raw candidate imports in deploy,
  machine, volume, domain, serving, runtime, and ACME modules fail the boundary
  check.
- `scripts/check-boundary.sh`: raw substrate type aliases and public re-exports
  from product modules fail the boundary check.
- `crates/ployz/src/facts/mod.rs`: product fact encoding rejects malformed
  product payloads with structured product errors.

### Unit 4. Machine Membership Projection Consumer

Files:

- `crates/ployz/src/machine.rs`
- `crates/ployz/src/facts/machine.rs`
- `crates/ployz-e2e/src/scenarios/machine_add.rs`

Work:

- Define Ployz-owned machine membership fact payloads for joined, removing, and
  tombstoned lifecycle events.
- Implement a machine reducer that produces `MachineStatus` from verified
  machine facts.
- Add a `MachineMembershipPort` adapter backed by Polis facts/projection.
- Keep `MachineMembershipService` unchanged at the product level where
  possible: it should still observe, diff, and join.

Tests:

- `crates/ployz/src/machine.rs`: first add appends one joined fact and returns
  `Joined`.
- `crates/ployz/src/machine.rs`: already-present reads projection state and
  does not append a fresh fact.
- `crates/ployz/src/machine.rs`: conflicting machine identity remains a
  structured `MembershipConflict`.
- `crates/ployz/src/facts/machine.rs`: full projection rebuild from joined
  facts returns the same `MachineStatus`.
- `crates/ployz-e2e/src/scenarios/machine_add.rs`: product scenario uses the
  fact-backed adapter without importing raw Polis candidate types.

### Unit 5. Domain And Serving Projection Consumer

Files:

- `crates/ployz/src/domain/mod.rs`
- `crates/ployz/src/serving/mod.rs`
- `crates/ployz/src/deploy/mod.rs`
- `crates/ployz/src/facts/domain.rs`
- `crates/ployz/src/facts/serving.rs`
- `crates/ployz-e2e/src/scenarios/domain_add.rs`
- `crates/ployz-e2e/src/scenarios/https_deploy.rs`

Work:

- Define Ployz-owned domain status facts for pending certificate issuance,
  pending serving activation, ready, and failed states.
- Define Ployz-owned serving commit facts for committed route/hostname/target
  generation.
- Implement reducers that produce `DomainStatus` and serving commit views.
- Make the domain status adapter record facts and read projections.
- Make deploy write/observe serving facts through the product `ServingPort`,
  then catch up to the serving fact cursor before live activation verification.
- After catch-up, re-read the typed serving projection and assert the exact
  route, hostname, target, and generation identity before live activation can
  satisfy deploy success.
- Keep live serving activation acknowledgement as a `ServingPort` observation,
  not as Polis truth.

Tests:

- `crates/ployz/src/domain/mod.rs`: certificate in-progress records pending as
  a domain fact and does not record failed.
- `crates/ployz/src/domain/mod.rs`: ready domain fact rebuilds into
  `DomainStatus::Ready`.
- `crates/ployz/src/serving/mod.rs`: serving commit projection preserves route,
  hostname, target, and generation identity.
- `crates/ployz/src/deploy/mod.rs`: deploy does not report success until the
  serving projection has caught up and live activation is acknowledged.
- `crates/ployz/src/deploy/mod.rs`: deploy rejects catch-up where the cursor is
  advanced but the typed serving projection does not contain the exact
  committed route, hostname, target, and generation.
- `crates/ployz-e2e/src/scenarios/https_deploy.rs`: retry after coordinator
  crash observes domain/serving facts and finishes missing runtime or activation
  work without attempt replay.

### Unit 6. Volume Ownership And Cleanup Validation

Files:

- `crates/ployz/src/volume/mod.rs`
- `crates/ployz/src/facts/volume.rs`
- `crates/ployz-e2e/src/scenarios/volume_transfer.rs`

Work:

- Define Ployz-owned volume ownership facts and cleanup pending facts.
- Include submitted fence metadata in ownership fact append requests.
- Implement reducers that produce `OwnershipObservation` and `CleanupStatus`.
- Keep source stop, snapshot, final delta, receive, ownership commit, and
  cleanup side effects as product/backend work.
- Validate that the same Polis fact/projection substrate supports a second
  unlike state machine without adding volume-shaped concepts to Polis.

Tests:

- `crates/ployz/src/volume/mod.rs`: desired ownership observed from facts
  returns without rerunning source/target mutations.
- `crates/ployz/src/volume/mod.rs`: cleanup pending remains visible after
  ownership transfer and rebuild.
- `crates/ployz/src/facts/volume.rs`: conflicting ownership facts produce a
  structured stale/conflict product outcome.
- `crates/ployz-e2e/src/scenarios/volume_transfer.rs`: second run observes
  ownership and cleanup status through projection.

## Sequencing

1. Build Polis fact core with in-memory store.
2. Build Polis projection source and catch-up over the in-memory store.
3. Add the Ployz adapter boundary and boundary checks.
4. Convert machine membership to a fact/projection-backed adapter as a small
   preparatory consumer.
5. Convert domain/serving deploy state to fact/projection-backed adapters and
   treat HTTPS deploy as the first real proof gate.
6. Validate volume ownership and cleanup on the same substrate as the second
   unlike proof.
7. Only after those slices pass review, write a backend adapter plan for
   persistent/synced storage.

Each implementation slice should end with focused tests, `just check`,
`cargo clippy --workspace --all-targets -- -D warnings`, a zero-context review
when API surface changes, then commit and push.

## Risks

- **Product seepage into Polis:** If a second unlike domain cannot use a type
  without awkward wrappers, the type belongs in Ployz.
- **Projection mistaken for live truth:** Projection catch-up proves visibility,
  not runtime, serving, or ACME success. Product code must keep live
  verification.
- **Backend premature commitment:** Pulling in p2panda, iroh-docs, or NATS
  before the API proof risks copying MVP complexity before the boundary is
  clear.
- **Raw candidates leaking upward:** Product modules should not branch on
  candidate statuses. Adapters and reducers translate candidates into product
  status.
- **Authority shortcuts:** Replica import authority, fact author authority, and
  fact-key grants must stay distinct when persistent/sync backends arrive.
- **Background reconciler drift:** Projection rebuild and catch-up may be used
  by foreground commands or supervised roles, but they must not silently mutate
  durable product truth.

## Completion Criteria

- Polis has product-neutral fact and projection substrate modules with no Ployz
  imports.
- HTTPS deploy/domain/serving passes through the substrate and remains
  observable, typed, and live-verified.
- At least one second unlike Ployz product domain, preferably volume ownership
  and cleanup, uses the same substrate without adding product-shaped concepts
  to Polis.
- Ployz product modules use typed ports and views, not raw candidate status.
- Product modules cannot import, type-alias, or re-export raw substrate types.
- Machine add, HTTPS deploy/domain serving state, and volume ownership remain
  observable operations, not attempts or workflows.
- Projection rebuild and catch-up are explicit, tested, and do not infer live
  success.
- Fact append authorization is resource/key/kind-aware and separate from
  broad scope authorization, replica import authority, and future author-key
  trust.
- `just check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `scripts/check-boundary.sh` pass.
