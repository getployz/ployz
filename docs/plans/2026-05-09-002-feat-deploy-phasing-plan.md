---
title: "feat: Deploy phasing, checkpoints, and tiered rollout primitives"
type: feat
status: active
date: 2026-05-09
origin: user request after docs/plans/2026-05-09-001-feat-zfs-volume-move-execution-plan.md
---

# feat: Deploy phasing, checkpoints, and tiered rollout primitives

## Summary

Introduce deploy phases as explicit orchestration units with ordered work,
checkpointed commit boundaries, and rollback semantics. The immediate goal is
not a cloud-side rollout policy engine. It is a core deploy primitive that can
represent "do database work first, commit that checkpoint, then roll web" and
later support tiered or recurring rollout commands without hiding work behind a
background reconciler.

The key shift is that a deploy is no longer one opaque apply block ending in one
commit. It becomes a plan of named phases. Each phase has participants, work
items, verification, and a commit policy. Some phases are reversible until the
deploy commits. Other phases, like a database upgrade or volume ownership move,
become durable checkpoints before later phases run. If a later web phase fails,
the result must say "database checkpoint committed; web phase failed" rather
than pretending the entire deploy rolled back.

## Problem Frame

The current deploy executor already has an internal startup phase concept:
`run_phase_startup` groups service starts by `PlannedService.phase`, and tests
cover phase ordering in `crates/ployz-orchestrator/src/deploy/tests.rs`. But
that phase is only a startup queue detail. It does not model:

- multiple commit points,
- irreversible or non-rollbackable work,
- phase-specific verification,
- tiered rollout waves,
- resumable rollout commands,
- or recurring rollout schedules driven by cloud/CLI/operator surfaces.

The ZFS move slice made this sharper. Some work, such as a volume move or DB
upgrade, can be safely completed and committed before web traffic moves. Other
work should remain reversible until a later switch. The deploy result and stored
state need to make that explicit.

## Requirements

- R1. Deploy planning produces an ordered phase plan, not just an unordered set
  of service and volume changes.
- R2. A phase is a first-class unit of work with an id, name, order,
  participants, work items, verification requirements, commit policy, and
  rollback policy.
- R3. The executor must be able to commit selected phase outcomes before later
  phases run.
- R4. A failed later phase must not rewrite or imply rollback of an earlier
  irreversible checkpoint.
- R5. Tiered rollouts, such as `db` before `web`, must be representable without
  cloud-specific override logic in core.
- R6. Recurring or gradual rollouts must be resumable explicit operations, not
  background reconciliation loops.
- R7. Existing manifest users keep default behavior: absent phase hints, deploys
  behave like the current single transaction where practical.
- R8. Preview and apply output must expose phase boundaries, commit points, and
  residual rollback limits.
- R9. The model must keep the cloud as a consumer. Cloud may render phase hints
  into deploy manifests or call resume commands, but core decides what work is
  valid and what state transitions are durable.

## Scope Boundaries

- This plan does not implement a full cloud scheduler.
- This plan does not implement traffic-splitting percentages for gateways unless
  needed as a later rollout wave primitive.
- This plan does not require automatic rollback. Rollback remains an explicit
  command, and irreversible checkpoints can make rollback partial or disallowed.
- This plan does not add a reconciler that wakes up and advances phases on its
  own.
- This plan does not solve schema migration authoring. It models when migration
  work runs and commits, not how an app generates migrations.

## Existing Context

- `crates/ployz-types/src/spec.rs` defines `DeployManifest`, `DeployIntent`,
  `ServiceIntentHint`, and `VolumeIntentHint`. This is the right place for
  portable phase hints once the shape is chosen.
- `crates/ployz-orchestrator/src/deploy/plan.rs` already gives
  `PlannedService` an internal `phase: Option<u32>`. Today, creates/replaces
  default to phase `0`, removals have no startup phase, and the phase is not a
  public manifest concept.
- `crates/ployz-orchestrator/src/deploy/execute.rs` runs a single apply flow:
  inspect, final plan stability, write applying status, execute volume moves,
  start candidates, build one commit plan, `commit_deploy`, status writes,
  certificate work, cleanup.
- `crates/ployz-orchestrator/src/deploy/lifecycle.rs` has the current commit
  boundary: `PreparedDeploy -> StartedCandidates -> CommitPlan ->
  CommittedDeploy`.
- `crates/ployz-types/src/model.rs` has `DeployState::{Planning, Applying,
  Committed, CleanupPending, Failed}` and `DeployApplyResult.events`, but no
  phase-level status or partial-commit state.
- `docs/residual-review-findings/codex-zfs-volume-move-execution.md` records
  the need for reversible writer quiescing and transfer cancel/interrupt in
  future slices; deploy phases should make those semantics easier to express.

## Key Technical Decisions

- Make phase semantics explicit in the deploy domain model before expanding the
  manifest surface. The first implementation should build phases from existing
  plan data and internal defaults, then expose manifest hints once the executor
  can honor them.
- Keep phase advancement command-shaped. A recurring or gradual rollout is a
  series of explicit apply/resume operations against a stored phase cursor, not
  a daemon loop that advances itself.
- Separate phase completion from durable commit. A phase may finish work but
  defer its state mutation to a later commit group, or it may create a
  checkpoint that is committed immediately.
- Model rollback policy as phase data. Rollback decisions should branch on
  structured `RollbackPolicy`, not infer reversibility from service names or
  event text.
- Treat database/volume checkpoints as durable operation facts. Once a DB phase
  commits, later web failure should result in a failed or paused deploy with a
  committed checkpoint, not a lie that the DB work rolled back.

## Proposed Model

### Phase Plan

Add a deploy planning model shaped roughly like:

```rust
pub struct DeployPhasePlan {
    pub phase_id: DeployPhaseId,
    pub name: String,
    pub order: u32,
    pub work: Vec<DeployPhaseWork>,
    pub participants: BTreeSet<MachineId>,
    pub commit_policy: PhaseCommitPolicy,
    pub rollback_policy: PhaseRollbackPolicy,
    pub advance_policy: PhaseAdvancePolicy,
}
```

The exact Rust shape belongs to implementation, but the semantic fields matter:

- `phase_id`: stable within a deploy plan and suitable for resume commands.
- `work`: service starts/stops, volume moves, service moves, portal/branch work,
  migration commands, route changes, cleanup.
- `commit_policy`: whether successful work commits at phase completion or waits
  for a later group.
- `rollback_policy`: whether the work can be reverted by rollback, requires a
  follow-up command, or is irreversible.
- `advance_policy`: whether the next phase starts immediately, waits for an
  operator resume, waits for a time window supplied by the caller, or repeats as
  a rollout wave.

### Commit Policy

Start with a small enum:

- `EndOfDeploy`: current default. Work is staged and committed with the final
  deploy commit.
- `Checkpoint`: successful phase writes durable state before later phases run.
  Later failure cannot roll this back implicitly.
- `NoStoreCommit`: operational work that produces events or observations but
  does not mutate deploy-owned durable state.

Future expansion can add named commit groups, but this first version should
avoid a generalized transaction DSL.

### Rollback Policy

Start with:

- `Reversible`: rollback can restore the prior release/routing/volume owner with
  an explicit rollback command.
- `ForwardOnly`: rollback cannot undo the phase; operator must roll forward or
  run a specific compensating command.
- `External`: phase ran an external command or migration whose reversal is not
  modeled by ployz.

DB upgrades should usually be `ForwardOnly` or `External`. A web release switch
can be `Reversible`. A volume move may become `Reversible` only after the
rollback primitive has enough ZFS lineage evidence to move or reattach safely.

### Advance Policy and Recurrence

Recurring or tiered rollout support should be explicit and resumable:

- `Immediate`: run next phase in the same apply call.
- `Manual`: stop after this phase and return a result that says which phase is
  next.
- `Windowed`: caller provides an allowed execution window; core validates and
  either runs or pauses.
- `Wave { batch }`: run one rollout wave and persist a cursor; a later
  `deploy resume` advances the next wave.

There should be no daemon loop that wakes up to advance a rollout. Cloud can
schedule "resume phase X at 10:00" by calling the core operation. CLI users can
do the same manually or via their own scheduler. Core owns correctness,
idempotency, and state transitions.

## Example Semantics

### DB Before Web

A deploy manifest or cloud-rendered plan can produce:

- Phase `db-prep`: run/replace `db`, run optional migration command, verify DB
  readiness, `commit_policy = Checkpoint`, `rollback_policy = ForwardOnly`.
- Phase `web-rollout`: replace `web` slots, verify readiness,
  `commit_policy = EndOfDeploy`, `rollback_policy = Reversible`.

If `db-prep` succeeds and commits, but `web-rollout` fails, the deploy record
should expose something like:

- deploy state: `failed_after_checkpoint` or `paused_failed`
- committed phases: `db-prep`
- failed phase: `web-rollout`
- rollback limit: DB checkpoint is not automatically rolled back

The exact state enum needs design, but the operator-facing truth must be this
clear.

### Tiered Web Rollout

A rollout can split web slots into waves:

- Phase `web-wave-1`: start 1 slot, verify, pause or continue.
- Phase `web-wave-2`: start next 25%, verify, pause or continue.
- Phase `web-wave-final`: start remaining slots, commit release.

The core phase cursor makes this safe to retry. Cloud can choose to resume each
wave automatically, but the cluster only changes when a concrete resume command
runs.

### Recurring Maintenance Rollout

A recurring rollout should be stored as an operator/cloud schedule outside the
deploy plan, but each occurrence creates or resumes an explicit deploy phase:

- "Every Tuesday, advance one region" becomes: cloud calls
  `deploy resume --phase region-wave --window ...`.
- The core validates the stored cursor and phase preconditions, runs one unit,
  and returns a result.
- If the call is skipped, nothing changes in the cluster.

## Implementation Units

### U1. Introduce Phase Domain Types and Preview Shape

**Goal:** Add a first-class phase plan model without changing execution
behavior yet.

**Files:**

- Modify: `crates/ployz-types/src/model.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/plan.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**

- Add deploy preview types for phases, commit policy, rollback policy, and
  advance policy.
- Teach `ResolvedPlan` to produce a default single phase from current service
  and volume work.
- Preserve existing `DeployPreview.services` and `volume_moves` for API
  compatibility; add `DeployPreview.phases` as additive structured evidence.
- Keep existing internal `PlannedService.phase` but rename or wrap it so it is
  clearly startup-wave data inside a `DeployPhasePlan`.

**Test Scenarios:**

- A basic manifest produces one default phase with `EndOfDeploy` and
  `Reversible`.
- A volume move appears as phase work before service startup work in preview.
- Existing preview tests still pass with additive phase evidence.

### U2. Add Phase Result and Checkpoint State Records

**Goal:** Store phase completion and commit boundaries durably enough for retry,
resume, and honest failure reporting.

**Files:**

- Modify: `crates/ployz-types/src/model.rs`
- Modify: `crates/ployz-store-api/src/lib.rs`
- Modify: memory/NATS store implementations under `crates/ployz-nats/` and
  test support in `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**

- Add a `DeployPhaseRecord` keyed by `namespace`, `deploy_id`, and `phase_id`.
- Store status: `pending`, `running`, `succeeded`, `checkpoint_committed`,
  `failed`, `skipped`.
- Store commit evidence and rollback policy for completed phases.
- Add list/get/upsert APIs to the deploy store.
- Do not mutate service release state yet; this unit only makes phase status
  durable.

**Test Scenarios:**

- Phase record round-trips through memory store.
- Rewriting a running phase to succeeded preserves phase id and deploy id.
- A checkpoint phase records rollback policy and commit evidence.

### U3. Refactor Apply Into Phase Executor

**Goal:** Make each phase its own execution unit while preserving current
single-commit behavior by default.

**Files:**

- Modify: `crates/ployz-orchestrator/src/deploy/execute.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/lifecycle.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**

- Extract the current apply flow into explicit stages: preflight, prepare,
  execute phase, commit group, post-commit cleanup.
- Keep one default phase initially so behavior remains stable.
- Write `DeployPhaseRecord` before and after each phase execution.
- Ensure phase execution remains foreground work and returns structured errors.
- Keep `ensure_plan_stable` before phase execution; later slices can decide
  whether a resume command revalidates only the remaining phases.

**Test Scenarios:**

- Current deploy happy path writes a succeeded default phase and committed
  deploy.
- Current deploy failure writes a failed phase and failed deploy.
- Retry of a failed deploy does not treat stale phase records from another
  deploy id as success.

### U4. Implement Checkpoint Commit Policy

**Goal:** Allow a phase to commit durable state before later phases run.

**Files:**

- Modify: `crates/ployz-orchestrator/src/deploy/lifecycle.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/execute.rs`
- Modify: `crates/ployz-types/src/model.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**

- Split commit planning so a `CommitPlan` can be built for a subset of phase
  work.
- Add a deploy state or deploy summary evidence for "checkpoint committed,
  later phase not complete." Candidate names: `PartialCommitted` or
  `FailedAfterCheckpoint`. The implementation should choose the smallest state
  extension that lets callers branch without parsing warnings.
- Ensure a later phase failure preserves earlier committed release/volume
  records and phase records.
- Add result fields that report committed checkpoints and failed phase.

**Test Scenarios:**

- DB checkpoint phase commits its release before web phase starts.
- Web phase failure after DB checkpoint leaves DB release committed and marks
  deploy as failed-after-checkpoint or equivalent structured state.
- Retrying the deploy recognizes the committed checkpoint and does not rerun it
  unless the manifest hash or phase fingerprint changes.

### U5. Expose Manifest Phase Hints

**Goal:** Let deploy manifests express core phase intent such as DB before web
without cloud-only overrides.

**Files:**

- Modify: `crates/ployz-types/src/spec.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/plan.rs`
- Test: `crates/ployz-types/src/spec.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**

- Add `DeployIntent.phases` or a top-level `phases` field. Prefer intent if the
  shape remains planner guidance rather than desired steady state.
- Define each phase with `id`, optional `name`, ordered `services`, `volumes`,
  optional `after`, `commit_policy`, `rollback_policy`, and `advance_policy`.
- Validate that every referenced service/volume exists and that the phase graph
  is acyclic.
- Default unmentioned work into the final phase.
- Keep cloud free to render these hints, but make the core validation and
  planning authoritative.

**Test Scenarios:**

- Manifest with `db` phase before `web` phase resolves in that order.
- Manifest rejects duplicate phase ids.
- Manifest rejects unknown service/volume references.
- Manifest rejects cycles in `after`.
- Unmentioned services fall into default phase with current behavior.

### U6. Add Resume/Pause Surface for Manual and Wave Rollouts

**Goal:** Support tiered and recurring rollout without background advancement.

**Files:**

- Modify: `crates/ployz-orchestrator/src/deploy/mod.rs`
- Modify: `crates/ployzd/src/daemon/handlers/deploy.rs`
- Modify: CLI parsing/rendering in `crates/ployzd/src/main.rs` and
  `crates/ployzd/src/cli_io.rs` if deploy subcommands are defined there
- Test: `crates/ployzd/src/daemon/handlers/deploy.rs`
- Test: relevant CLI tests in `crates/ployzd/src/main.rs`

**Approach:**

- Add a `deploy resume` operation that takes `namespace`, `deploy_id`, and
  optional `phase_id`.
- Resume validates the stored phase cursor, plan fingerprint, participants, and
  phase preconditions.
- `Manual`, `Windowed`, and `Wave` advance policies pause after a bounded unit
  of work and return the next phase cursor.
- Cloud or user cron can call `deploy resume`; core never advances phases on a
  timer by itself.

**Test Scenarios:**

- Manual phase pauses after checkpoint and reports next phase.
- Resume advances the next phase exactly once.
- Resume rejects if manifest/plan fingerprint changed.
- Windowed phase rejects outside caller-supplied allowed window without
  mutating state.

### U7. Phase-Aware Rollback Preconditions

**Goal:** Make rollback honest when earlier checkpoints are non-reversible.

**Files:**

- Modify: rollback planning code once present, or add placeholders in
  `crates/ployz-orchestrator/src/deploy/execute.rs` if rollback is still
  future work
- Modify: `crates/ployz-types/src/model.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**

- Rollback command reads committed phase records and rollback policies.
- If all committed phases are reversible, rollback can plan a normal restoration.
- If any committed phase is `ForwardOnly` or `External`, rollback returns a
  structured rejection unless the caller requests an explicit partial rollback
  mode.
- Include phase ids and reasons in the error.

**Test Scenarios:**

- Rollback is allowed after a reversible web-only deploy.
- Rollback rejects after a DB checkpoint marked `ForwardOnly`.
- Partial rollback reports exactly what remains committed.

## Sequencing

1. U1 phase preview model.
2. U2 durable phase records.
3. U3 executor refactor with one default phase.
4. U4 checkpoint commit policy.
5. U5 manifest phase hints for DB before web.
6. U6 resume/pause surface for tiered and recurring rollout.
7. U7 phase-aware rollback preconditions.

This order keeps behavior stable while carving the executor into clearer units.
The first three units should be PR-able without changing user-visible deploy
semantics except additive preview/status evidence.

## Open Questions

- Should partial checkpoint completion be a new `DeployState` variant
  (`PartialCommitted`, `FailedAfterCheckpoint`) or a separate phase-status
  surface while deploy remains `Failed`? The plan leans toward a structured
  state extension because callers should not parse warnings.
- Should DB migration commands be modeled as service work, volume work, or a
  separate `DeployPhaseWork::Command`? The plan should defer this until command
  execution surfaces are clearer.
- Should `commit_policy = Checkpoint` be allowed for arbitrary services, or only
  for services/volumes explicitly marked as forward-only? The safer first slice
  is to allow it only when rollback policy is explicit.
- How much phase evidence belongs in `DeployRecord.summary_json` versus a new
  store table? The plan proposes both preview evidence and durable phase records
  so operators can inspect current status without replaying summary JSON.

## Verification Strategy

- Keep `cargo test -p ployz-orchestrator` as the main inner loop for planning
  and executor changes.
- Run `cargo test -p ployz-types` when manifest or model schema changes.
- Run `cargo test -p ployzd` when daemon request/CLI/resume surfaces change.
- Run `cargo check --workspace` before PRs that touch public API or store
  traits.
- Add targeted tests before broad refactors: phase preview tests first, then
  phase record tests, then executor behavior tests.

## Rollout Notes

- All new manifest fields should be additive and optional.
- Existing manifests should continue to produce the current single-phase deploy.
- Store/API changes should use additive fields and new records where possible.
- If a new `DeployState` variant is added, treat it as public API and update CLI
  rendering and SDK consumers in the same PR.
