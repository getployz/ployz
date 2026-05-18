---
title: Slice 018b p2panda Fact Substrate Plan
status: planned
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution.md
  - MVP/slice-018a-p2panda-substitution-investigation.md
  - MVP/slice-018-deploy-restart-recovery-plan.md
---

# Slice 018b p2panda Fact Substrate Plan

## Problem Frame

Slice 018a proved that `p2panda-core`, `p2panda-store`, and
`p2panda-stream` can carry the generic fact-substrate work Ployz should not
maintain by hand: signed operation envelopes, body-hash verification,
append-only author logs, local operation storage, and ingestion validation.

The next slice should turn that spike into a production-shaped MVP-local
substrate:

```text
p2panda operations/store/stream -> Ployz FactSource candidates -> existing reducers
```

The goal is not to rewrite the product around p2panda. The goal is to delete or
avoid custom substrate while preserving the Ployz semantics that matter:
NATS-shaped bus behavior, explicit foreground commands, deterministic reducers,
operator-visible conflicts, gateway/DNS snapshots, WireGuard application, and
deploy commit-before-drain.

## Requirements Trace

- `VISION.md`: Ployz should expose explicit operational primitives, not hide
  behavior in a controller or generic framework.
- `MVP/overall-plan.md`: the next implementation slice should adopt
  `p2panda-core`, `p2panda-store`, and `p2panda-stream` behind `FactSource`
  before deploy restart recovery hardens the old fact boundary.
- `MVP/design-notes/p2panda-substitution.md`: p2panda adoption is biased
  positive but must remain behind a reversible adapter until E2E proofs pass.
- `MVP/primitive-decisions.md`: `FactSource` is the projection-facing read seam;
  reducers remain Ployz-owned business logic.
- `MVP/slice-018-deploy-restart-recovery-plan.md`: deploy recovery should use
  one fact substrate for deploy decision, serving commit, and cleanup facts.
- The current spike in `MVP/p2panda-spike` proves the p2panda operation fit but
  is explicitly not production substrate.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`:
  prove authorization and compatibility before mutating replicated authority or
  storage state.
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`:
  keep durable truth, static metadata, and live observations separate.
- `docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md`:
  separate local mutation paths from remote propagation paths.
- `docs/solutions/performance-issues/machine-add-timeout-tests-2026-05-10.md`:
  use operation-scoped test policies for p2p/storage deadlines rather than
  global timeout hacks.

## Scope

In scope:

- Add a production-shaped `mvp-p2panda-facts` crate under `MVP/`.
- Move the useful spike behavior into the new crate and leave
  `MVP/p2panda-spike` as deleted or obsolete.
- Implement a p2panda-backed writer for Ployz fact payloads.
- Implement a p2panda-backed `FactSource`.
- Preserve conflict-as-candidate semantics.
- Preserve payload read by content hash.
- Preserve fact read/write authorization through the existing
  `FactAuthorizer`.
- Preserve existing projection reducers unchanged.
- Add focused unit tests for write/read/conflict/auth/missing-payload behavior.
- Add at least one E2E scenario proving existing reducers can rebuild SQLite and
  serving snapshots from the p2panda-backed source.
- Update `MVP/primitive-decisions.md`, `MVP/e2e-proof-plan.md`, and
  `MVP/design-notes/p2panda-substitution.md` after implementation.

Out of scope:

- Replacing PloyzBus.
- Replacing projection reducers.
- Replacing deploy, machine, ACME, routing, serving, DNS, or WireGuard business
  logic.
- Adopting `p2panda-net`, `p2panda-discovery`, or `p2panda-blobs`.
- Replacing iroh transport or iroh-docs sync everywhere in one step.
- Implementing `p2panda-auth` membership. That is a follow-up slice after the
  fact adapter exists.
- Implementing deploy restart recovery. This slice prepares the substrate that
  recovery should use.
- Touching code outside `MVP/`.

## Crate Scout

Already completed in `MVP/design-notes/p2panda-substitution.md`.

Decision for this slice:

- Adopt `p2panda-core = 0.5.2` for signed operation bodies and BLAKE3 content
  hashes.
- Adopt `p2panda-store = 0.5.2` for local operation/log storage.
- Adopt `p2panda-stream = 0.5.2` for ingestion validation and persistence.
- Do not adopt `p2panda-net` because it depends on an older iroh line than the
  current MVP.
- Do not adopt `p2panda-blobs` because the crates.io 0.5.2 package is not a
  usable blob API.
- Keep the dependency in a narrow `mvp-p2panda-facts` crate so replacing or
  upgrading p2panda later does not touch business crates.

## Design Decisions

### New Crate Boundary

Create:

```text
MVP/p2panda-facts/Cargo.toml
MVP/p2panda-facts/src/lib.rs
```

Package name:

```text
mvp-p2panda-facts
```

The crate may depend on:

- `mvp-bus` for `FactKey`, `FactKeyPattern`, `FactPayload`, `FactAuthorizer`,
  `BusSession`, `IslandId`, `PrincipalId`, and content hashes.
- `mvp-projection` for `FactSource`, `FactCandidate`, `CandidateStatus`, and
  `classify_fact_key`.
- `p2panda-core`, `p2panda-store`, and `p2panda-stream`.

It should not depend on deploy, routing, machine, ACME, serving, mesh, or E2E.

### Writer Contract

Introduce a writer with a small outcome enum:

```text
PandaFactWriter
  write_fact_payload(author, key, payload, authorizer) -> PandaFactWriteOutcome

PandaFactWriteOutcome
  Inserted(metadata)
  AlreadyPresent(metadata)
  Conflict(metadata)
```

The semantics should match the current immutable fact contract:

- same island + same key + same content hash is `AlreadyPresent`;
- same island + same key + different content hash is `Conflict`;
- unauthorized writer returns a structured error before mutation;
- conflict detection must not require read grants, only write authority for the
  writer and local substrate visibility.

Do not silently overwrite a same-author p2panda log entry for the same Ployz
fact key. The current iroh-docs wrapper needed explicit immutable preflight
because raw docs writes are key-overwrite shaped; the p2panda adapter should
make immutable operation writes the default.

The writer owns local mutation only. Remote propagation and future p2p sync are
separate paths. Do not make a local write wait for remote visibility, and do not
hide remote propagation failure inside the local write result.

### Operation Shape

Use p2panda operations as the durable fact envelope.

Header extensions should carry:

- island id;
- fact key;
- Ployz principal id;
- optional future fact kind/epoch cache only if implementation becomes clearer.

The operation body carries the fact payload. The body hash becomes
`FactContentHash` in `b3:<hex>` form.

For now, use one p2panda log id per island. Keep prefix scanning in a simple
Ployz-owned index inside the adapter or by scanning the local p2panda store.
Do not prematurely design a separate index database until scale tests show this
path is hot.

### Author Identity

For this slice, keep both identities explicit:

- p2panda public key: cryptographic operation author;
- Ployz principal id: authorization and operator identity.

The adapter must bind public key to principal when writing locally and when
importing operations for tests. It must reject or mark unverified operations
whose public key has no local principal binding.

Do not collapse `PrincipalId` into p2panda public key yet. That decision belongs
with the future `p2panda-auth` island membership slice.

Before writing, the adapter must preflight:

- the principal has write authority for the fact key;
- the author key is bound to that principal locally;
- the p2panda operation version/extensions are supported by this node.

This is the fact-substrate version of preflight-before-mutation: fail before
writing an operation that the local node already knows it should reject.

### Reader Contract

Implement `FactSource` for the p2panda-backed store:

```text
list_candidates(island, pattern, session)
read_payloads(island, candidates, session)
```

Candidate statuses:

- `Verified`: operation validates, author binding exists, read grant allows the
  fact, payload hash matches.
- `Conflict`: operation validates and read grant allows it, but there are
  multiple authorized content hashes for the same key and the fact kind is one
  the reducers expect as conflict-as-candidate.
- `Unauthorized`: operation validates and has an author binding, but read grant
  denies access.
- `Unverified`: operation validation fails, payload is absent, payload hash is
  wrong, or author binding is missing.
- `CrossIsland`: extension island does not match the requested island.

The reader should not parse payloads into `ProjectionFactPayload`. It only
produces `FactCandidate` and payload bytes; reducers continue to own payload
decoding.

### Missing Payloads

p2panda allows operation headers without bodies in some flows. For Ployz facts,
payload absence is not success.

Rules:

- A header with missing body but non-zero payload size becomes `Unverified`.
- `read_payloads` only returns payloads whose bytes are present and match the
  candidate content hash.
- Projection and command readers must see missing payloads as unavailable
  evidence, not as a stale prior payload.

### Existing Substrates During Migration

Keep existing sources temporarily:

- `BusFactSource` remains useful for narrow bus/projection tests and scale
  tests.
- `IrohDocsFactSource` remains useful for existing docs-backed proofs until
  those scenarios are moved or replaced.
- `ProcessFactSource` remains a process-role harness until the process harness
  has a p2panda-backed replacement.

This slice should add a better production-shaped substrate, not delete every old
fixture in one commit.

### Truth Versus Observation

The p2panda operation log is durable fact truth. Projection state, SQLite rows,
snapshot files, local operation indexes, sync status, and peer reachability are
observations or derived state.

Do not let the adapter silently rewrite fact truth based on projection status,
sync health, peer liveness, or retry loops. Background sync may report
observations; foreground commands write facts after explicit authorization and
preflight.

### Deadline/Test Policy

If the adapter needs waiting behavior, such as waiting for payload availability
or future sync catch-up, expose it as an operation-scoped policy. Do not add
global sleeps or hard-coded production deadlines to make tests pass.

## Implementation Units

### U1: Production Crate Skeleton

Files:

- Create `MVP/p2panda-facts/Cargo.toml`
- Create `MVP/p2panda-facts/src/lib.rs`
- Modify `MVP/Cargo.toml`
- Eventually remove or mark obsolete `MVP/p2panda-spike`

Approach:

- Move the spike's useful operation-author-store shape into the new crate.
- Rename spike-oriented types to production names.
- Keep the public API narrow: author, store, writer outcome, source adapter,
  errors.

Test scenarios:

- New crate compiles independently.
- `cargo test -p mvp-p2panda-facts --lib` runs focused tests.

Verification:

- `cargo clippy -p mvp-p2panda-facts --all-targets -- -D warnings`

### U2: Immutable p2panda Fact Writer

Files:

- `MVP/p2panda-facts/src/lib.rs`

Approach:

- Write p2panda operations with Ployz metadata in header extensions and payload
  bytes in the body.
- Use `p2panda-stream::operation::ingest_operation` for validation and store
  persistence.
- Return explicit inserted/already-present/conflict outcomes.
- Enforce fact write authorization before mutation.

Test scenarios:

- First write returns `Inserted`.
- Same key/same payload returns `AlreadyPresent`.
- Same key/different payload returns `Conflict` and does not overwrite the
  original operation.
- Unauthorized writer returns structured error and no candidate appears.
- Unsupported operation version/extension returns structured error before local
  mutation.
- Multiple authors writing different payloads for the same key both remain
  visible to the reader as candidates.

Verification:

- `cargo test -p mvp-p2panda-facts --lib writer`

### U3: FactSource Adapter

Files:

- `MVP/p2panda-facts/src/lib.rs`

Approach:

- Implement `FactSource` over the local p2panda store.
- Preserve read authorization through `FactAuthorizer`.
- Preserve conflict-as-candidate semantics using `classify_fact_key` and
  current reducer expectations.
- Keep payload reads keyed by `FactContentHash`.

Test scenarios:

- Verified operation becomes `CandidateStatus::Verified`.
- Same-key conflicting operations become visible as conflict candidates where
  the reducer expects conflict candidates.
- Unauthorized read becomes `CandidateStatus::Unauthorized` and payload is not
  returned.
- Unknown author binding becomes `CandidateStatus::Unverified`.
- Cross-island metadata becomes `CandidateStatus::CrossIsland`.
- Missing payload becomes `CandidateStatus::Unverified`.
- Payload hash mismatch is rejected or marked unverified.
- Local operation indexes do not become durable truth; deleting/rebuilding the
  index from p2panda operations produces the same candidates.

Verification:

- `cargo test -p mvp-p2panda-facts --lib source`

### U4: Projection E2E Contract

Files:

- Modify `MVP/e2e/Cargo.toml`
- Create or modify `MVP/e2e/src/p2panda_fact_source_contract.rs`
- Modify `MVP/e2e/src/main.rs`
- Update `MVP/e2e-proof-plan.md`

Approach:

- Add a scenario that writes node/service/serving facts through
  `mvp-p2panda-facts`, runs the existing projection actor against the
  p2panda-backed `FactSource`, deletes/rebuilds SQLite, and verifies
  gateway/DNS snapshot output.
- Keep the scenario small but real enough to prove reducers consume the new
  substrate.

Test scenarios:

- Projection sees p2panda-backed node/service facts.
- Projection sees a p2panda-backed serving commit.
- SQLite projection rebuild after delete succeeds.
- Gateway/DNS snapshots are written from p2panda-backed facts.
- Unauthorized and conflict candidates are counted the same way as the existing
  projection contract.

Verification:

- `cargo run -p mvp-e2e -- p2panda-fact-source-contract`
- `MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all`

### U5: Routing/Deploy Writer Compatibility

Files:

- Modify `MVP/routing/src/lib.rs` only if a generic fact-writer seam is needed.
- Modify `MVP/deploy/src/facts.rs` only if the deploy fact writer can cleanly
  consume the p2panda writer outcome.
- Add focused tests in `MVP/deploy/src/tests.rs` or
  `MVP/routing/src/lib.rs`.

Approach:

- Do not force deploy or routing to depend directly on p2panda unless the seam
  is small and clearer than an adapter.
- Prefer a generic immutable fact writer adapter if both serving and deploy
  facts need the same write contract.
- Keep command code branchable on inserted/already-present/conflict.

Test scenarios:

- Serving commit write can be expressed through the p2panda-backed writer or a
  generic wrapper.
- Deploy decision write can be expressed through the p2panda-backed writer or a
  generic wrapper.
- Conflict errors remain structured and do not become stringly backend errors.

Verification:

- `cargo test -p mvp-routing -p mvp-deploy`

### U6: Documentation And Decision Ledger

Files:

- Modify `MVP/primitive-decisions.md`
- Modify `MVP/design-notes/p2panda-substitution.md`
- Modify `MVP/slice-018-deploy-restart-recovery-plan.md`
- Create `MVP/slice-018b-p2panda-fact-substrate.md`

Approach:

- Record what p2panda now owns and what remains Ployz-owned.
- Update the deploy restart recovery plan so it uses the p2panda-backed fact
  source/writer instead of docs-backed wording.
- Include any deletion/avoidance evidence from replacing or retiring the spike.

Verification:

- Docs point to real crate/tests that exist.
- The next deploy restart plan has no stale "docs-backed fact source" language
  where p2panda is now the chosen substrate.

## Sequencing

1. U1 and U2 first: crate skeleton and immutable writes.
2. U3 next: `FactSource` adapter and reader semantics.
3. U4 after U3: E2E projection contract.
4. U5 after U4 if deploy/routing compatibility needs a shared writer seam.
5. U6 last: decision ledger and completion report.

Do not start deploy restart recovery until this slice's E2E contract passes and
the recovery plan has been rewritten around the resulting p2panda fact boundary.

## Verification Gate

Minimum gate before shipping:

```text
cargo fmt --all
cargo clippy -p mvp-p2panda-facts --all-targets -- -D warnings
cargo test -p mvp-p2panda-facts --lib
cargo test -p mvp-routing -p mvp-deploy
cargo run -p mvp-e2e -- p2panda-fact-source-contract
MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
git diff --check
```

Run the simplify workflow after the first green `mvp-p2panda-facts` unit-test
pass and before the E2E contract is finalized.

Run code review with subagents after the full gate passes. At minimum, review
with:

- correctness reviewer for fact conflict/auth semantics;
- security reviewer for author binding and unauthorized payload leakage;
- maintainability/simplicity reviewer for adapter shape.

## Risks

- `p2panda-store` does not provide Ployz fact-prefix queries. The adapter may
  need a small Ployz-owned index or store scan. Start with the simplest scan and
  measure before adding indexing.
- p2panda is pre-1.0. Keep adoption behind `mvp-p2panda-facts` so API churn is
  localized.
- Public-key identity and Ployz principal identity are not unified. Keep the
  mapping explicit until the p2panda-auth membership slice resolves it.
- A too-generic writer seam could recreate framework complexity. Keep writer
  methods narrow and driven by current serving/deploy needs.
- Keeping old fact sources around temporarily can confuse maintainers. The
  completion report should label each remaining source as production substrate,
  migration fixture, or test harness.
- If local writes and remote propagation get coupled too early, commands will
  start inheriting hidden quorum behavior. Keep the operator's connected node as
  the consistency boundary until an explicit future membership/partition slice
  changes that.
