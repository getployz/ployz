---
title: "refactor: Replace CommandRunner With Polis Attempt Primitive"
type: refactor
status: active
date: 2026-05-22
origin: goal
depends_on:
  - docs/brainstorms/2026-05-21-polis-ployz-boundary-requirements.md
  - docs/architecture/polis-mvp-extraction-map.md
  - docs/plans/2026-05-21-007-goal-polis-ployz-rust-idiomatic-api-plan.md
---

# refactor: Replace CommandRunner With Polis Attempt Primitive

## Problem Frame

The current Polis/Ployz split is directionally correct, but the operation
boundary still makes Ployz carry framework bookkeeping. Product modules issue
commands by manually building command kinds, fingerprint resources, payload
hashes, and mutation intents. `CommandRunner` centralizes some lifecycle work,
but Ployz still owns replay branching, terminal marker policy, checkpoint
encoding, and broad primitive failure mapping.

The goal of this refactor is to replace `CommandRunner` with a Polis
`Attempt` primitive. Polis should make distributed-system mechanics explicit
and hard to misuse: authorization proof, idempotency, replay state, canonical
fingerprinting, evidence recording, terminalization, and first-class
distributed outcomes. Ployz should keep the product story: claim, verify,
mutate, prove, finish.

This does not hide the fact that Ployz is a distributed system. Equal nodes,
lease loss, stale fences, in-progress work elsewhere, freshness unknown,
interrupted attempts, replay, and lost certainty remain visible in Ployz. The
bookkeeping that should disappear is raw fingerprint construction, terminal
marker selection, evidence byte encoding, and ad hoc failure mapping.

## Requirements Trace

- From the goal: replace `CommandRunner` with an `Attempt` primitive.
- From the goal: Polis owns authorization proof, idempotency, replay state,
  canonical fingerprinting, evidence recording, and terminalization.
- From the goal: terminalization is RAII. Dropping an unfinished attempt
  auto-terminalizes as interrupted.
- From the goal: distributed outcomes such as replay, lease loss, stale fence,
  freshness unknown, in-progress elsewhere, and interrupted are first-class
  enums at call sites, not flattened `Error::Conflict` style variants.
- From the goal: Ployz owns product sequence, verification, and product errors.
- From the goal: product files stop importing `MutationIntent`,
  `CommandPayload`, `FingerprintedResource`, and `CommandKind`.
- From the goal: fold in audit fixes around submitted fence fingerprint shape,
  typed submitted fence fingerprints, structured module errors, and explicit
  projection scope.
- From the goal: `DeployCommand::issue` should collapse from roughly forty
  lines of manual command assembly to roughly six lines of product code.
- From the goal: adding `machine add` should require roughly sixty lines of
  business logic and no manual payload/fingerprint construction.
- From the goal: two commands must not silently disagree on failure mapping.
- From the goal: `ensure_X` and `verify_X` mirror methods should merge or pair
  through a trait shape where practical.
- From the boundary requirements: Polis must stay product-neutral and must not
  know deploy, ACME, serving, volume, or machine workflows.
- From the MVP extraction map: evidence is not truth; product verifiers decide
  whether replayed evidence proves the product invariant.

## Key Decisions

### D1. Polis Exposes `Attempt`, Ployz Removes `CommandRunner`

Polis will expose a product-neutral `Attempt` lifecycle API. Ployz will no
longer have a sealed `CommandBackend`/`CommandRunner` layer. Product engines
will depend on an attempt log/service object and an issued product command
token.

The important boundary is:

- Polis: issue authorization, request identity, idempotent start/replay,
  canonical fingerprint, evidence write, terminal marker write, RAII
  interrupted close, and typed operation outcomes.
- Ployz: product command type, product request, product verifiers, product
  evidence enum, and product failure mapping.

### D2. Attempts Are Explicit, Not A Workflow Engine

The first implementation should not introduce a step graph or workflow DSL.
Ployz should keep normal Rust control flow. Polis gives an open attempt value
and operations on that value:

- `attempt.context()` to access mutation context.
- `attempt.prove(evidence)` to record product-named evidence safely.
- `attempt.succeeded(outcome)` to consume and close successfully.
- `attempt.failed(failure)` or `attempt.interrupted(reason)` to consume and
  close non-successfully.
- `Drop` closes unfinished open attempts as interrupted.

If a later slice proves that volume transfer needs per-step effect permits,
that can be added as a narrow extension. It is not part of the initial
replacement.

### D3. Canonical Fingerprints Move To Polis

Ployz product code should describe command identity through typed fields.
Polis should own canonical encoding and digesting. The command identity API
should make command name, version, resources, submitted fence, authority epoch,
and payload fields explicit without exposing byte formatting to product
modules.

The digest should replace the current hand-rolled `u64` hash with a durable
canonical digest. If no external digest dependency is already present, add a
small well-known dependency such as `sha2` at the workspace level.

### D4. Submitted Fence Is Not Structural Fingerprint Payload

Submitted fence should not be a generic string blob embedded inside
`RequestFingerprint` as loose structural payload. Polis should model it as a
typed product-neutral value:

- typed resource id,
- holder,
- epoch,
- claim hash,
- stable digest/identity contribution.

`RequestFingerprint` should compare the final canonical fingerprint. It may
expose submitted fence metadata for tests and diagnostics, but the structural
request identity should not be defined by an optional boxed stringly fence
object.

### D5. Distributed Outcomes Are Structured

Polis errors should split by module and carry context. Operation lifecycle
outcomes should be ordinary enums:

- `AttemptReplay::Succeeded`
- `AttemptReplay::InProgressElsewhere`
- `AttemptReplay::Failed`
- `AttemptReplay::Interrupted`
- `AttemptFailure::LeaseLost`
- `AttemptFailure::FenceStale`
- `AttemptFailure::FreshnessUnknown`
- `AttemptFailure::TerminalizationFailed`

Ployz maps these into product errors through a common mapping shape so deploy,
volume, and future machine commands cannot silently disagree on lifecycle
semantics.

### D6. Replayed Success Requires Product Verification

Polis may know that a previous attempt wrote a success terminal marker. That
does not prove a deploy, domain, runtime participant, serving activation, or
volume transfer remains valid. Product replay handling must call a verifier
before returning success.

The operation boundary should make this pairing explicit so `ensure_X` and
`verify_X` are either one trait method with a mode or a pair on the same trait
with shared request/receipt types.

## Overall LFG Slice Plan

Each sub-slice is intended to be independently reviewable. At the end of each
slice:

1. Run the focused crate tests for the touched code.
2. Run `just check`.
3. Run `cargo clippy --workspace --all-targets -- -D warnings`.
4. Run a zero-context design/code review for changed API surfaces when the
   slice is large enough to warrant it.
5. Fix accepted findings or record durable residuals.
6. Commit and push the branch.

## Sub-Slice A: Polis Attempt Core

### Scope

Introduce the product-neutral attempt primitive in Polis while preserving
existing behavior through compatibility tests. This slice does not migrate
Ployz engines yet.

### Files

- `crates/polis/src/lib.rs`
- `crates/polis/src/operations.rs`
- `crates/polis/src/error.rs`
- `crates/polis/Cargo.toml`
- `Cargo.toml`

### Work

- Add Polis operation module error variants with structured context rather than
  relying on one flat `Error` enum for all failure classes.
- Add `AttemptIssue`, `AttemptSpec`, `AttemptStart`, `AttemptReplay`,
  `OpenAttempt`, `AttemptContext`, and attempt terminal APIs.
- Add RAII interruption for dropped open attempts. The best-effort drop path
  must be observable in tests and must not mask explicit terminalization
  failures on `succeeded`, `failed`, or `interrupted`.
- Add canonical command fingerprint construction in Polis. Product callers
  should be able to add typed scalar fields and typed resources without
  constructing opaque bytes.
- Replace the existing submitted fence structural field with typed submitted
  fence fingerprinting.
- Keep existing `OperationBackend` fakes working or replace them with a
  renamed `AttemptBackend` that preserves start/replay/record/close semantics.

### Tests

- `crates/polis/src/operations.rs`: same idempotency and identical canonical
  fingerprint replays as success/open/failed/interrupted according to backend
  terminal state.
- `crates/polis/src/operations.rs`: same idempotency and different canonical
  fingerprint returns structured conflict.
- `crates/polis/src/operations.rs`: explicit success consumes the attempt and
  records one success terminal marker.
- `crates/polis/src/operations.rs`: dropping an open attempt records one
  interrupted terminal marker.
- `crates/polis/src/operations.rs`: explicit terminalization failure returns a
  structured terminalization failure instead of being ignored.
- `crates/polis/src/operations.rs`: submitted fence identity contributes to
  canonical fingerprinting without exposing a stringly fence payload as the
  fingerprint structure.

### Acceptance

- Polis can start, replay, prove, and terminalize attempts without Ployz
  `CommandRunner`.
- Polis owns canonical fingerprint digesting.
- Operation lifecycle outcomes are visible as structured enums.

## Sub-Slice B: Ployz Operation Boundary Migration

### Scope

Replace `CommandRunner`, `CommandBackend`, `CommandEnvelope`,
`MutationIntent`, `CommandPayload`, `CommandKind`, and
`FingerprintedResource` with a thin Ployz operation boundary that issues
product commands through Polis attempts.

### Files

- `crates/ployz/src/operation/mod.rs`
- `crates/ployz/src/operation/command/mod.rs`
- `crates/ployz/src/operation/command/issue.rs`
- `crates/ployz/src/operation/command/run.rs`
- `crates/ployz/src/operation/context.rs`
- `crates/ployz/src/operation/polis_boundary.rs`
- `crates/ployz/src/operation/claims.rs`
- `crates/ployz/src/operation/identity.rs`

### Work

- Replace command runner exports with attempt-oriented exports:
  `AttemptLog`, `AttemptContext`, `AttemptIssue`, `IssuedAttempt`, and
  product command helpers.
- Delete or retire `CommandRunner` tests after equivalent attempt tests exist.
- Keep product-facing `MutationContext` only if it carries product-relevant
  mutation identity; otherwise use Polis `AttemptContext` through a Ployz
  wrapper.
- Move failure mapping to a common structured lifecycle conversion so product
  modules cannot invent inconsistent replay/open/interrupted mapping.
- Keep direct Polis imports inside `crates/ployz/src/operation/` only.

### Tests

- `cargo test -p ployz operation`
- Ployz operation boundary tests prove that replay states map to one shared
  lifecycle enum.
- Ployz operation boundary tests prove product code can record typed evidence
  without constructing raw bytes.
- Ployz operation boundary tests prove failure terminalization failures are
  surfaced where explicit terminalization is attempted.

### Acceptance

- `rg "CommandRunner|CommandBackend|MutationIntent|CommandPayload|CommandKind|FingerprintedResource" crates/ployz/src`
  returns no product-facing usage. Any remaining matches must be migration
  tests or deleted code in the same slice.
- Product modules no longer import internal command/fingerprint construction
  types.

## Sub-Slice C: Deploy And Domain Attempt Integration

### Scope

Move HTTPS deploy and domain readiness onto the new attempt API. The product
flow should read as product orchestration, with replay verification explicit.

### Files

- `crates/ployz/src/deploy/mod.rs`
- `crates/ployz/src/domain/mod.rs`
- `crates/ployz/src/acme/mod.rs`
- `crates/ployz/src/runtime/mod.rs`
- `crates/ployz/src/serving/mod.rs`
- `crates/ployz/src/error.rs`
- `crates/ployz-e2e/src/scenarios/https_deploy.rs`
- `crates/ployz-e2e/src/scenarios/domain_add.rs`
- `crates/ployz-e2e/src/scenarios/coordinator_restart.rs`

### Work

- Collapse `DeployCommand::issue` to a short product-level call that declares
  command type, version, resources, and typed payload fields through Polis
  fingerprint helpers.
- Replace `DeployEngine<O: CommandBackend>` with an attempt-backed dependency.
- Make terminal-success replay pair with product verification in one trait
  shape. For example, introduce a small trait or helper that makes
  `ensure_ready` and `verify_ready` explicit counterparts sharing request and
  output types.
- Preserve typed deploy failures: certificate unusable reason, domain failure,
  runtime failure, serving failure, and operation lifecycle failure should not
  collapse into broad buckets.
- Keep domain and deploy product code free of raw Polis operation imports.

### Tests

- `cargo test -p ployz`
- `cargo test -p ployz-e2e --test scenarios https_deploy` if the crate exposes
  filtered integration tests; otherwise `cargo test -p ployz-e2e`.
- Existing HTTPS deploy success still writes one success terminal marker.
- Certificate unusable reason survives through deploy error mapping.
- Runtime failure cause survives through deploy error mapping.
- Terminal-success replay verifies domain, runtime, and serving without
  rerunning mutating work.
- Interrupted/open/failed replay is product-visible and does not run product
  verification as success.

### Acceptance

- `DeployCommand::issue` is roughly six lines of product declaration, not a
  manual command assembly function.
- Deploy product code reads as domain readiness, runtime activation, serving
  activation, outcome.
- Deploy and domain still expose distributed uncertainty where it matters.

## Sub-Slice D: Volume Transfer Attempt Integration

### Scope

Move volume transfer onto the attempt API while preserving MVP safety checks:
claim/current owner checks before dangerous mutation, product verification
after participant calls, cleanup pending visibility, and explicit replay
handling.

### Files

- `crates/ployz/src/volume/mod.rs`
- `crates/ployz/src/error.rs`
- `crates/ployz-e2e/src/scenarios/volume_transfer.rs`
- `docs/architecture/polis-mvp-extraction-map.md` only if the implementation
  discovers a missing grounded invariant.

### Work

- Replace `O: CommandBackend` with an attempt dependency.
- Collapse `VolumeTransferCommand::issue` to product declaration using Polis
  canonical fingerprint helpers.
- Preserve submitted fence participation through the typed fence fingerprint
  shape.
- Keep product flow explicit: require current claim, stop writes, require
  current claim, snapshot, require current claim, final delta, require current
  claim, receive, require current claim, commit ownership, verify ownership,
  prove, cleanup, finish.
- Retain cleanup pending reason instead of dropping it.
- Keep replay-success verification through product ports.

### Tests

- `cargo test -p ployz volume`
- `cargo test -p ployz-e2e --test scenarios volume_transfer` if available;
  otherwise `cargo test -p ployz-e2e`.
- Stale claim rejects before source mutation and before later mutation.
- Same-idempotency success replay verifies committed ownership without source
  mutation.
- Same-idempotency interrupted replay returns interrupted and does not rerun
  mutation.
- Cleanup failure outcome includes the artifact and reason.
- Submitted fence changes affect canonical command identity.

### Acceptance

- Volume product code no longer imports raw command/fingerprint construction
  types.
- Volume still visibly handles distributed safety: fences, stale claims,
  ownership verification, replay, and cleanup uncertainty.

## Sub-Slice E: Structured Errors And Projection Scope

### Scope

Finish the audit fixes that are not naturally completed by the attempt
migration: structured module errors, explicit projection scope, and shared
operation lifecycle mapping.

### Files

- `crates/polis/src/error.rs`
- `crates/polis/src/operations.rs`
- `crates/polis/src/claims.rs`
- `crates/ployz/src/error.rs`
- `crates/ployz/src/deploy/mod.rs`
- `crates/ployz/src/domain/mod.rs`
- `crates/ployz/src/volume/mod.rs`
- any new projection/read module introduced by the attempt migration

### Work

- Split Polis failure domains so operation, claim, authority, and malformed
  payload failures carry structured context.
- Add Ployz product lifecycle error wrappers so deploy and volume share the
  same replay/open/interrupted mapping.
- Declare projection scope explicitly anywhere a product verifier reads
  existing state. If no projection API exists yet, capture this as a named
  verifier input rather than adding speculative projection substrate.
- Remove any remaining `ensure_X`/`verify_X` duplication where the two methods
  can be represented as a trait with `ensure` and `verify` sharing request,
  proof, and failure types.

### Tests

- Errors are branchable without parsing display strings.
- Deploy and volume operation lifecycle mapping uses the same shared source
  enum.
- Product replay verifiers receive explicit scope/context.
- No new public projection substrate appears without a real product caller.

### Acceptance

- Two commands cannot disagree silently on lifecycle failure mapping.
- Projection scope is explicit at verifier/read call sites.
- Errors carry structured context through product boundaries where callers need
  to branch.

## Sub-Slice F: Machine Add API Proof

### Scope

Add a minimal machine-add product command skeleton as an API proving ground.
The goal is not full machine provisioning. The goal is to verify that a new
command can be expressed as concise business logic without manual
payload/fingerprint construction.

### Files

- `crates/ployz/src/lib.rs`
- `crates/ployz/src/machine.rs` or `crates/ployz/src/machine/mod.rs`
- `crates/ployz/src/error.rs`
- `crates/ployz-e2e/src/scenarios/machine_add.rs` if an e2e scenario module is
  appropriate

### Work

- Add a small machine-add request, outcome, ports, and engine that use the new
  attempt API.
- Keep business logic around sixty lines by relying on shared attempt issuing,
  lifecycle, fingerprinting, and evidence APIs.
- Do not implement real machine provisioning unless the current codebase
  already has the required adapters in the root rewrite.
- Use the skeleton to find remaining friction in command issue ergonomics.

### Tests

- Machine add issues an attempt without manual payload/fingerprint construction
  in the product module.
- Success closes the attempt once.
- Success replay calls product verification rather than mutation.
- Open/interrupted/failed replay maps through shared lifecycle mapping.

### Acceptance

- Machine add demonstrates the intended API shape for future commands.
- Any remaining boilerplate is either removed immediately or recorded as a
  residual API issue before marking the goal complete.

## Sub-Slice G: Final API Review, Simplification, And Docs

### Scope

Run a final zero-context API review and simplify anything that no longer earns
its place after the attempt migration.

### Files

- `crates/polis/src/lib.rs`
- `crates/polis/src/operations.rs`
- `crates/polis/src/claims.rs`
- `crates/ployz/src/operation/`
- `crates/ployz/src/deploy/mod.rs`
- `crates/ployz/src/domain/mod.rs`
- `crates/ployz/src/volume/mod.rs`
- `crates/ployz/src/machine.rs` or `crates/ployz/src/machine/mod.rs`
- `docs/architecture/polis-mvp-extraction-map.md`
- this plan

### Work

- Remove dead compatibility types and tests from the previous command runner
  API.
- Check all public API exports for framework nouns that do not prove a value
  or permit a narrower action.
- Update the MVP extraction map only for newly learned grounded invariants.
- Mark this plan completed only when tests, review, commits, and pushes have
  all landed.

### Tests

- `cargo test -p polis`
- `cargo test -p ployz`
- `cargo test -p ployz-e2e`
- `just check`
- `cargo clippy --workspace --all-targets -- -D warnings`

### Acceptance

- `CommandRunner` is gone.
- Product files no longer import `MutationIntent`, `CommandPayload`,
  `FingerprintedResource`, or `CommandKind`.
- Polis owns distributed operation bookkeeping.
- Ployz product code reads as explicit distributed product logic.
- A zero-context reviewer agrees the split is coherent or all residual
  disagreements are durable in docs.

## Verification Matrix

| Slice | Focused tests | Full gates |
| --- | --- | --- |
| A | `cargo test -p polis` | `just check`; clippy |
| B | `cargo test -p ployz operation` | `just check`; clippy |
| C | `cargo test -p ployz`; deploy/domain e2e | `just check`; clippy |
| D | `cargo test -p ployz`; volume e2e | `just check`; clippy |
| E | `cargo test -p polis`; `cargo test -p ployz` | `just check`; clippy |
| F | `cargo test -p ployz machine` or full `cargo test -p ployz` | `just check`; clippy |
| G | full workspace tests | `just check`; clippy |

## Risks

- RAII terminalization can hide failures if it is treated as reliable cleanup.
  Explicit terminalization APIs must return errors; `Drop` is only a
  best-effort interrupted marker.
- A step-aware attempt API could become a workflow engine. Keep the initial
  design to an open attempt plus proof/terminal methods.
- Typed evidence storage can become product leakage if Polis imports product
  receipt types. Product evidence stays in Ployz and converts to
  product-neutral evidence at the boundary.
- Machine-add skeleton can drift into real provisioning scope. Keep it as an
  API proof unless adapters already make the real operation trivial.

## Non-Goals

- No repo split in this goal.
- No full machine provisioning implementation unless needed to prove the API.
- No speculative projection or record substrate without a real product caller.
- No attempt to make distributed failure invisible to Ployz.
- No compatibility shim for the old `CommandRunner` API after the migration is
  complete.
