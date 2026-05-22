---
title: "refactor: Reframe Polis Around Observable Cluster Operations"
type: refactor
status: active
date: 2026-05-22
origin: goal
supersedes:
  - docs/plans/2026-05-22-001-refactor-polis-attempt-primitive-plan.md
depends_on:
  - VISION.md
  - docs/brainstorms/2026-05-21-polis-ployz-boundary-requirements.md
  - docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md
  - legacy/mvp/architecture.md
  - legacy/mvp/slice-024-acme-command-surface-plan.md
  - legacy/mvp/slice-054-product-deploy-command-plan.md
---

# refactor: Reframe Polis Around Observable Cluster Operations

## Problem Frame

The previous plan over-generalized Polis `Attempt` into the central operation
runner for Ployz. That was the wrong boundary. Deploy, machine membership, and
volume movement are cluster-observable operations: after a coordinator crash,
the next command should read current facts, projections, and live participant
state, compute what remains true or missing, and apply a bounded diff. It
should not replay a workflow transcript or trust generic evidence as product
truth.

Polis should instead be the small set of foundational distributed-system
capabilities that let Ployz product code stay direct:

- fact store and projection substrate,
- advisory leases with fencing tokens,
- bounded request/reply for peer coordination,
- authority and grants,
- a narrow `Attempt` primitive only for external-state operations that are not
  fully observable from cluster state, with ACME issuance as the canonical
  first target.

Ployz remains the product layer. It owns deploy manifests, machine membership,
volume transfer semantics, ACME product rules, runtime policy, serving rules,
failure classification, and command result shape.

## Requirements Trace

- From the current goal: Reframe Polis/Ployz around observable cluster
  operations.
- From the current goal: Polis keeps fact store/projection, advisory leases,
  bounded request/reply, authority/grants, and a narrow `Attempt` primitive
  only for external-state operations like ACME issuance.
- From the current goal: Remove Attempt/CommandRunner-style orchestration from
  deploy, machine add, and volume transfer.
- From the current goal: Observable operations read current cluster state,
  compute desired state, diff, apply bounded mutations, and return structured
  results without replay-from-evidence.
- From `VISION.md`: Ployz exposes explicit primitive operations, has no
  background reconcilers, reports live state on demand, and treats half-applied
  success as the worst failure class.
- From `docs/brainstorms/2026-05-21-polis-ployz-boundary-requirements.md`:
  Polis must not know deploy, ACME, serving, volume, or machine workflows; Ployz
  feature code must not mention raw projection candidates, fact-log
  import/export, backend watch mechanics, or lease reduction outside adapters.
- From `legacy/mvp/architecture.md`: durable facts and projections are the
  state model, leases are advisory facts with real exclusivity enforced by the
  protected resource, and deploy commits are local durable facts with eventual
  replication.
- From `docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md`:
  deploy should read stored lifecycle intent at invocation time and make
  movement previewable/applyable; it must not become a background reconciler.
- From `legacy/mvp/slice-054-product-deploy-command-plan.md`: deploy should
  coordinate participants, write durable serving facts, project them, and drain
  old runtime only after projection catch-up.
- From `legacy/mvp/slice-024-acme-command-surface-plan.md`: ACME is the
  advisory-lease-fenced singleton canary and the likely place where an external
  attempt primitive earns its keep.

## Scope

In scope:

- Keep the already-built Polis attempt core only as a narrow primitive for
  external-state work.
- Remove the Ployz `operation/command` runner shape from deploy and volume.
- Do not keep the uncommitted machine-add Attempt prototype.
- Introduce or firm up Ployz-facing observable-operation ports for deploy,
  machine membership, and volume transfer.
- Make deploy read current namespace state, compute desired state from the
  request, produce a diff/plan, and execute bounded mutations.
- Make machine add and volume transfer use facts/projections, leases, and
  request/reply where those are the actual substrate.
- Move ACME issuance toward the narrow Attempt proof, while ACME challenge
  ownership remains lease/fact-shaped.
- Add tests that fail if deploy, machine add, or volume transfer depend on
  `AttemptLog`, terminal markers, or evidence replay.

Out of scope:

- A generic workflow engine.
- Background reconciliation.
- Commit quorum or witness acknowledgement for fact writes.
- Real repo split into separate Polis and Ployz repositories.
- Full production ACME protocol integration unless a later slice explicitly
  reaches it.

## Key Decisions

### D1. Attempt Is Not The Product Operation Boundary

`Attempt` is for external-state work where local cluster observation cannot
reconstruct enough truth to safely decide the next action. ACME issuance and
external certificate directory interactions fit this model because rate limits,
orders, authorizations, and account state live outside the cluster.

Deploy, machine add, and volume transfer do not fit this model. Their durable
truth lives in cluster facts, projections, leases, runtime participants, and
resource-specific backends. Their recovery model is observation plus diff, not
evidence replay.

### D2. Observable Operations Are Plain Rust Services

Observable Ployz operations should read as ordinary Rust:

```text
let _lease = leases.acquire(resource)?;
let current = observe(...)?;
let desired = desired_from(request, &current)?;
let plan = diff(&current, &desired)?;
execute(plan)?;
```

There should be no closure passed to a generic runner, no terminal marker
selection, and no replay verifier for these operations.

### D3. Facts And Projections Are The Cluster Memory

Polis owns the product-neutral substrate: candidate status, fact source/read
APIs, reducer traits, rebuild mechanics, freshness metadata, and watch
plumbing. Ployz owns product payload enums, reducers, views, and command
interpretation.

Command recovery reads this state directly. If state is ambiguous, the command
returns structured ambiguity. It does not infer success from a generic evidence
record.

### D4. Leases Are Guards, Not Workflow State

Leases remain advisory coordination facts with fencing tokens. They should be
used where the protected resource has an actual fencing point: ACME challenge
ownership, volume ownership mutation, and contested namespace updates. A lease
guard proves "this holder may attempt this fenced mutation now"; it does not
prove the whole product operation completed.

### D5. Request/Reply Is For Participants

Bounded request/reply belongs in Polis as a narrow distributed communication
capability. Ployz owns the request and response types. Runtime activation,
volume receive, drain, and serving reload should use bounded peer calls with
timeouts and structured failures.

### D6. Deploy Is Observe, Diff, Apply

Deploy does not have replay-from-evidence. A fresh deploy observes current
namespace state, computes desired state from the manifest, calculates a plan,
and executes the diff. If a previous deploy crashed after leaving orphaned
runtime or serving state, the next deploy should observe that state and include
cleanup in the new diff.

### D7. ACME Is The Narrow Attempt Proof

ACME challenge ownership can remain lease/fact-shaped. ACME issuance should be
the first narrow `Attempt` consumer because it crosses an external service where
local facts cannot fully observe the state machine. The `Attempt` API should be
judged by whether ACME code becomes safer and smaller, not by whether deploy
can be forced through it.

## Target Product Shapes

### Deploy

Target sketch:

```text
pub fn deploy_https(&self, request: DeployRequest) -> Result<DeployOutcome, DeployFailure> {
    let current = self.observe_namespace(&request.namespace)?;
    let desired = DeployDesiredState::from_request(&request, &current)?;
    let plan = DeployPlan::diff(&current, &desired)?;
    self.execute_plan(&plan)
}
```

`DeployPlan` should make runtime starts, route commits, serving commits, drains,
volume moves, certificate ensure steps, and cleanup explicit. The plan result is
structured; deploy does not emit or consume generic attempt evidence.

### Machine Add

Target sketch:

```text
pub fn add_machine(&self, request: MachineAddRequest) -> Result<MachineAddOutcome, MachineFailure> {
    let current = self.membership.observe(&request.machine)?;
    let desired = MachineDesiredState::joined(&request)?;
    let plan = MachineAddPlan::diff(&current, &desired)?;
    self.membership.apply(plan)
}
```

Existing membership should be a typed state, not a successful fresh mutation.
The command should return `AlreadyPresent`, `Joined`, or a structured conflict.

### Volume Transfer

Target sketch:

```text
pub fn transfer(&self, request: VolumeTransferRequest) -> Result<VolumeTransferOutcome, VolumeFailure> {
    let claim = self.leases.acquire(request.plan.volume.lease_resource())?;
    let current = self.ownership.observe(&request.plan.volume)?;
    let plan = VolumeTransferPlan::diff(&current, &request.plan)?;
    self.execute_transfer(plan, claim)
}
```

The transfer still uses leases and fencing at each mutation boundary, and still
uses request/reply for source and target participants. It does not use attempt
terminal markers to know whether ownership is current.

### ACME Issuance

Target sketch:

```text
pub fn ensure_certificate(&self, request: CertificateRequest) -> Result<CertificateOutcome, CertificateFailure> {
    let current = self.certs.observe(&request.binding)?;
    if current.is_usable_for(&request) {
        return Ok(current.into_outcome());
    }

    let attempt = self.attempts.begin(request.issue_attempt())?;
    let issued = self.acme.issue_or_resume(&attempt, &request)?;
    self.certs.activate(issued)?;
    attempt.succeeded()?;
    self.certs.observe_usable(&request)
}
```

This is the place to keep idempotency, canonical fingerprinting, external
operation evidence, and terminalization.

## Overall LFG Slice Plan

Each slice must end with:

1. Focused tests for touched crates.
2. `just check`.
3. `cargo clippy --workspace --all-targets -- -D warnings`.
4. Zero-context design/code review when the API surface changes substantially.
5. Fix accepted findings or record durable residuals.
6. Commit and push.

## Sub-Slice A: Stabilize The Direction And Remove Failed Prototype

### Scope

Create a clean baseline from the current branch by removing uncommitted
machine-add Attempt prototype code and documenting the new direction. Do not
delete the committed Polis attempt core yet; it will be narrowed later.

### Files

- `docs/plans/2026-05-22-002-refactor-polis-observable-operations-plan.md`
- `crates/ployz/src/machine.rs`
- `crates/ployz-e2e/src/scenarios/machine_add.rs`
- `crates/ployz-e2e/src/scenarios/mod.rs`
- any uncommitted deploy/volume/error edits from the abandoned prototype

### Work

- Keep this plan.
- Revert only the uncommitted failed prototype edits that make deploy, volume,
  and machine more Attempt-shaped.
- Preserve earlier committed Polis attempt core for now.
- Verify the branch returns to green before the first real observable-operation
  refactor begins.

### Tests

- `just check`
- `cargo clippy --workspace --all-targets -- -D warnings`

### Acceptance

- The branch has a clean, pushed plan commit.
- No uncommitted abandoned prototype code remains.
- Tests are green.

## Sub-Slice B: Remove Attempt Runner From Deploy

### Scope

Refactor deploy away from `IssuedDeployCommand`, `AttemptBackend`,
`AttemptLog`, replay verifiers, and terminalization. Deploy becomes an
observable operation over current state and plan execution.

### Files

- `crates/ployz/src/deploy/mod.rs`
- `crates/ployz/src/error.rs`
- `crates/ployz-e2e/src/scenarios/https_deploy.rs`
- `crates/ployz-e2e/src/scenarios/coordinator_restart.rs`
- `crates/ployz/src/operation/**` only if public exports need cleanup

### Work

- Replace `DeployCommand::issue` and `IssuedDeployCommand` with direct
  `DeployRequest` execution.
- Introduce `DeployObservedState`, `DeployDesiredState`, and `DeployPlan` only
  as much as needed to make the current HTTPS deploy path explicit.
- Remove deploy replay-success handling. Terminal interrupted replay should no
  longer be a deploy concept.
- Keep certificate ensure as a deploy step, but do not make deploy itself an
  attempt.
- Keep serving/runtime verification as plan execution checks.
- Update E2E tests to assert crash recovery through observed state, not
  terminal replay.

### Tests

- `crates/ployz/src/deploy/mod.rs`: desired state changes when route, workload,
  machine, serving target, or certificate window changes.
- `crates/ployz/src/deploy/mod.rs`: empty diff returns a structured no-op or
  already-current outcome.
- `crates/ployz-e2e/src/scenarios/https_deploy.rs`: successful HTTPS deploy
  still ensures certificate, starts runtime, commits serving, and verifies
  activation.
- `crates/ployz-e2e/src/scenarios/coordinator_restart.rs`: a retry after a
  partial serving commit observes current state and produces cleanup/finish
  work without replaying terminal markers.

### Acceptance

- `crates/ployz/src/deploy/mod.rs` does not mention `Attempt`, `AttemptLog`,
  `AttemptBackend`, `IssuedProductAttempt`, `AttemptTerminalMarker`, or replay.
- Deploy business logic remains readable and direct.
- HTTPS deploy behavior remains covered.

## Sub-Slice C: Remove Attempt Runner From Volume Transfer

### Scope

Refactor volume transfer into a lease/fence plus observable ownership operation.
The command should inspect current ownership, acquire or validate the transfer
lease, execute bounded participant mutations, commit ownership facts, and
record cleanup visibility without generic terminal markers.

### Files

- `crates/ployz/src/volume/mod.rs`
- `crates/ployz/src/error.rs`
- `crates/ployz-e2e/src/scenarios/volume_transfer.rs`
- `crates/ployz/src/operation/claims.rs`

### Work

- Remove `VolumeTransferCommand`, `IssuedVolumeTransferCommand`, and
  `AttemptBackend` from volume transfer.
- Keep `SubmittedFenceToken` or replace it with a clearer lease guard API.
- Keep per-mutation stale-claim checks before source stop, snapshot, final
  delta, receive, and ownership commit.
- Treat ownership verification and cleanup status as observed state, not replay
  status.
- Preserve cleanup-pending visibility.

### Tests

- Existing stale-claim-before-mutation tests continue to pass.
- Interrupted replay tests are removed or rewritten as "current ownership
  observed, no transfer work needed" or "cleanup still pending".
- Cleanup failure remains visible without rewriting ownership.
- Idempotent second run observes ownership and cleanup status without rerunning
  source/target mutation.

### Acceptance

- `crates/ployz/src/volume/mod.rs` does not mention `Attempt`, `AttemptLog`,
  `AttemptBackend`, terminal markers, or replay.
- Volume transfer still enforces fencing at mutation boundaries.
- The second run is idempotent through observation.

## Sub-Slice D: Add Machine Add As Observable Membership Operation

### Scope

Introduce machine add without using Attempt. Use the legacy MVP membership and
projection model as the reference: machine add writes or observes membership
facts, projects membership state, and returns a structured product result.

### Files

- `crates/ployz/src/machine.rs`
- `crates/ployz/src/lib.rs`
- `crates/ployz/src/error.rs`
- `crates/ployz-e2e/src/scenarios/machine_add.rs`
- `crates/ployz-e2e/src/scenarios/mod.rs`
- later slices may add `crates/ployz/src/projection/**` or fact ports

### Work

- Define `MachineId`, `MachineStatus`, `MachineAddRequest`,
  `MachineAddOutcome`, and a narrow membership port.
- Model add mutation as a typed result: `Joined`, `AlreadyPresent`, or
  structured conflict.
- Do not introduce `OperationId`, idempotency key, terminal marker, or evidence
  replay into machine add.
- Add E2E-style tests with fakes proving first add, already-present, and
  conflict behavior.

### Tests

- First add writes or applies exactly one membership mutation.
- Already-present returns `AlreadyPresent` without pretending a fresh mutation
  occurred.
- Conflicting machine identity returns structured conflict.
- Product module has no direct Polis imports.

### Acceptance

- Machine add is a small observable operation and does not import Polis attempt
  APIs.
- The code reads as membership observation plus bounded mutation.

## Sub-Slice E: Narrow Ployz Operation Module To Non-Observable Attempt Use

### Scope

Remove or rename the generic `operation/command` API so product code cannot
accidentally route observable operations through it. Keep only the pieces needed
for ACME or a future external-state operation.

### Files

- `crates/ployz/src/operation/mod.rs`
- `crates/ployz/src/operation/command/**`
- `crates/ployz/src/acme/mod.rs`
- `crates/ployz/src/domain/mod.rs`
- `crates/ployz/src/error.rs`

### Work

- Remove public exports that make `AttemptLog` look like the default operation
  runner.
- Rename the module if needed, for example from `command` to
  `external_attempt`.
- Keep identity, authority, claim guard, and mutation context where still
  useful.
- Ensure product modules other than ACME/domain certificate issuance cannot
  depend on the attempt runner by accident.

### Tests

- Boundary check fails on direct Attempt imports from deploy, machine, and
  volume.
- Existing domain/certificate tests still pass.
- Operation module unit tests cover attempt behavior only as an external-state
  primitive.

### Acceptance

- Ployz has no generic `CommandRunner`-style API.
- Attempt is discoverable as a narrow tool, not as the default orchestration
  shape.

## Sub-Slice F: ACME Attempt Proof

### Scope

Use ACME issuance or certificate ensure as the first real consumer of the narrow
Attempt primitive.

### Files

- `crates/ployz/src/acme/mod.rs`
- `crates/ployz/src/domain/mod.rs`
- `crates/ployz-e2e/src/scenarios/domain_add.rs`
- `crates/ployz-e2e/src/scenarios/https_deploy.rs`
- `crates/polis/src/operations.rs` if the attempt API needs a narrow polish

### Work

- Separate observable certificate status from external issuance attempt.
- Keep challenge ownership lease/fact-shaped.
- Use `Attempt` only around the external ACME issuance step where local state is
  insufficient.
- Ensure private key material and certificate material are never stored in
  generic evidence.
- Replay of a completed ACME attempt must still verify certificate usability
  before deploy can succeed.

### Tests

- Usable existing certificate returns without starting an attempt.
- Missing or unusable certificate starts an issuance attempt.
- Replay of an issued attempt verifies current certificate usability.
- Failed or interrupted issuance remains operator-visible and does not report
  deploy success.
- Generic attempt evidence never contains private key material.

### Acceptance

- Attempt is justified by external-state semantics.
- Deploy calls certificate ensure as a product step but does not become an
  attempt itself.

## Sub-Slice G: Polis Fact/Projection API Plan Follow-Up

### Scope

If deploy and machine add expose missing substrate pieces, write a follow-up
plan for fact store/projection extraction instead of expanding Attempt.

### Files

- `docs/plans/<next>-polis-fact-projection-substrate-plan.md`
- likely future implementation files under `crates/polis/src/**` and
  `crates/ployz/src/projection/**`

### Work

- Identify the smallest product-neutral projection API needed by deploy and
  machine add.
- Keep Ployz product payloads and reducers above the boundary.
- Do not add product-shaped facts to Polis.

### Tests

- Boundary tests proving Polis does not import Ployz.
- Reducer tests proving Ployz owns product interpretation.

### Acceptance

- Any new Polis substrate capability is backed by at least two unlike product
  consumers or is explicitly marked as not extraction-ready.

## Risks

- **Over-correcting away from Attempt:** ACME still needs a durable external
  operation story. Do not delete the Polis attempt core before the ACME proof
  has a replacement.
- **Reconciler drift:** "Observe, diff, apply" must remain foreground command
  behavior. Do not add loops that silently rewrite cluster truth.
- **Plan abstraction bloat:** `DesiredState` and `Plan` types should be added
  only where they make product code simpler and tests clearer.
- **Legacy mismatch:** The legacy MVP used p2panda and module names that may not
  survive in root crates. Copy semantics, not file layout.
- **Dirty worktree:** The current branch contains abandoned prototype edits.
  The first slice must cleanly separate plan-only commits from implementation
  cleanup.

## Completion Criteria

- Deploy, machine add, and volume transfer contain no
  `CommandRunner`/`AttemptLog`/terminal-marker orchestration.
- ACME or certificate issuance is the only Ployz product path using Attempt.
- Observable operations recover through current facts/projections/live state.
- Polis remains product-neutral and owns only foundational capabilities.
- Ployz product modules do not import `MutationIntent`, `CommandPayload`,
  `AttemptResource`, `AttemptKind`, or direct Polis attempt types.
- `just check` and `cargo clippy --workspace --all-targets -- -D warnings`
  pass.
- A zero-context review agrees the split no longer forces observable product
  operations through an external-attempt primitive.
