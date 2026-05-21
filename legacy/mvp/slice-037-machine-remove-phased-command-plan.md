---
title: Slice 037 Machine Remove PhasedCommand Migration Plan
status: active
created: 2026-05-19
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/phased-command.md
  - MVP/slice-036-phased-command-primitive-plan.md
  - MVP/slice-036-phased-command-primitive.md
external:
  - https://docs.rs/async-trait/latest/async_trait/
  - https://docs.rs/state-machines/latest/state_machines/
  - https://docs.rs/rs-statemachine/latest/rs_statemachine/
  - https://docs.rs/crate/async_fsm/0.1.4
---

# Slice 037 Machine Remove PhasedCommand Migration Plan

## Problem Frame

Slice 036 proved `mvp-commands` on environment promote/rollback. The next proof
should move a node-facing command onto the same primitive, because the end-state
requirement is stronger than "the command can resume": if the daemon dies,
steady-state serving, DNS, projections, and data-plane communication should keep
working, while new local modifications wait for the daemon/coordinator to
return.

Machine remove is the right next slice. It already has the exact manual pattern
that `PhasedCommand` exists to absorb:

- validate command entry before mutation,
- record visible nodes at decision time,
- write a durable remove decision,
- mark the node removal-started before serving cutover,
- commit serving facts before stop/tombstone,
- wait for projection catch-up before destructive cleanup,
- recover cleanup after coordinator outage,
- rebuild projection from facts and reject removed peers.

Today that shape is split across `execute_until_serving_commit`,
`finish_cleanup`, and `recover_pending_machine_remove_cleanup` in
`MVP/machine/src/remove.rs`, with additional p2panda command-phase glue living
locally in the environment E2E. That is useful proof code, but it is the next
maintenance-burden risk: a future machine add/remove/migrate command would have
to choose between the generic command primitive and the older bespoke recovery
helpers.

## Crate Scout

Checked on 2026-05-19:

- `state-machines` 0.9.0 provides macro-generated typestate machines, guards,
  callbacks, dynamic dispatch, and async support. It is aimed at transition
  correctness and Ruby-style state-machine ergonomics; it does not provide
  Ployz's durable fact-backed resume, projection-gated cleanup, or command
  fact conflict semantics.
- `rs-statemachine` 0.1.0 provides a builder-style state-machine engine with
  optional history, guards, timeouts, metrics, serde, and async action support.
  That is broader runtime machinery than this slice needs, and it would not
  remove the fact/projection/business boundary code where Ployz's complexity
  currently lives.
- `async_fsm` 0.1.4 is an async finite-state-machine engine built around Tokio
  `mpsc` event delivery and subscriptions. That is useful as a reference for
  async transition loops, but Ployz already has Kameo/bus actors and needs
  durable command facts, not another in-process event engine.
- `async-trait` 0.1.89 remains a good tool when dyn-compatible async traits are
  needed. `mvp-commands` currently avoids that dependency by using explicit
  boxed futures. This slice should keep that shape unless implementation shows
  a concrete readability win.

Decision: do not add a workflow or state-machine dependency for this slice.
Copy the useful idea only: each command phase is explicit and serializable, and
the runner handles phase history. Ployz-specific command facts, p2panda-backed
storage, projection catch-up, and node RPC semantics stay in MVP code.

## Scope

In scope:

- Migrate graceful machine remove onto `mvp-commands::run_phased`.
- Keep all changes inside `MVP/`.
- Add `mvp-machine`'s dependency on `mvp-commands`.
- Extract the p2panda command phase-store adapter out of
  `MVP/e2e/src/environment_branch_promote_rollback_contract.rs` into a narrow
  reusable MVP crate, tentatively `MVP/commands-p2panda`.
- Update environment and machine E2Es to use the reusable p2panda command phase
  store rather than carrying local copies.
- Keep machine remove facts as ordinary p2panda facts. Do not introduce a
  second persistence model.
- Preserve the existing no-quorum direction: local durable write returns,
  replication is eventual, and visible nodes at decision time remain evidence
  rather than a blocking quorum.
- Report semantic leverage after implementation: old/new command LOC,
  phase/recovery bookkeeping LOC, reusable adapter LOC, and E2E harness LOC.

Out of scope:

- No deploy migration in this slice.
- No machine add rewrite.
- No volume transfer migration.
- No new workflow engine, replay engine, queue, scheduler, timer service, or
  command registry.
- No iroh dependency changes.
- No Pingora/DNS production serving migration.
- No root-repo code movement or compatibility work outside `MVP/`.

## Requirements Traceability

- `VISION.md` requires explicit command primitives, boring steady state, and
  data-plane survival outside daemon orchestration. Machine remove exercises
  that requirement better than another environment-only command.
- `MVP/design-notes/phased-command.md` says to lift phase bookkeeping once at
  least three commands repeat it. Slice 036 created the primitive; this slice
  proves it outside environment promote/rollback.
- `MVP/e2e-proof-plan.md` tracks machine remove as a product proof for
  restart recovery, projection rebuild, and removed-peer rejection. This slice
  must preserve those behaviors while moving resume bookkeeping into
  `run_phased`.
- The operator direction after Slice 036 is to keep shrinking business-code
  choreography, not to preserve old code shape. This migration should delete or
  make private the bespoke orchestration surface rather than leave two
  equivalent public command paths.

## Maintenance-Burden Baseline

A read-only LOC investigation before this slice found that the MVP is reducing
product orchestration code, but shared substrate and E2E harnesses are now the
places to watch.

Current evidence:

- Old deploy broad path: about 14,438 nonblank/non-comment Rust LOC across old
  daemon deploy handlers, orchestrator deploy modules, NATS deploy store, and
  runtime deploy backends. MVP deploy is about 2,171 LOC excluding tests and
  about 3,354 with tests/adapters.
- Old machine path: about 7,499 LOC across old daemon machine handlers, join
  flow, machine policy, and NATS machine store. MVP machine plus
  machine-p2panda is about 3,513 LOC.
- Old volume path: about 5,052 LOC across old daemon volume handlers and
  runtime storage. MVP volume is about 1,070 LOC excluding tests.
- MVP core excluding E2E is still about 42,828 LOC, and including E2E about
  63,663 LOC. The rewrite is smaller than the old broad core, but the win is
  not automatic.
- Large MVP pressure points are `MVP/bus/src/memory.rs`,
  `MVP/p2panda-facts/src/lib.rs`, `MVP/projection/src/reducer.rs`, and
  `MVP/e2e/src/process_role_harness.rs`.

Implication for this slice: the target is not a cosmetic LOC decrease. The
target is fewer command-specific recovery paths and fewer local adapter copies.
If `mvp-machine` gains lines while E2E-local phase-store glue disappears and
machine remove becomes easier to resume/review, that can still be a good
trade. If the migration leaves the old split API and adds `PhasedCommand` on
top, it is a maintenance regression.

## Current Shape To Preserve

Preserve these machine-remove invariants:

- Command entry fails before mutation when the target is missing, already
  tombstoned, already removing, still active in the serving commit, or absent
  from the drain set.
- A no-responder prepare probe fails before any durable remove facts are
  written.
- `NodeRemovalStarted` is durable before serving cutover is committed.
- Stop/tombstone/cleanup-done happen only after the serving commit has been
  projected.
- If stop is unavailable after cutover, the command returns structured
  `CleanupPending` with a specific reason; it does not pretend the machine was
  removed.
- Recovery after coordinator outage does not replay the pre-cutover probe,
  decision write, removal-started write, or prepare/drain RPC.
- Rebuilding projections from facts removes the target from live nodes,
  tombstones it, and leaves no conflict status in the happy path.
- Removed peers are rejected by the mesh/WireGuard planning proof.

## Design

### Command Phase Map

The exact Rust enum can change during implementation, but the intended phase
shape is:

```text
Start
DecisionWritten { decision }
RemovalStartedWritten { decision, removal_started_fact_key }
Prepared { decision, removal_started_fact_key }
ServingCommitWritten { decision, removal_started_fact_key }
Stopped { decision, removal_started_fact_key }
Tombstoned { decision, removal_started_fact_key, tombstone_fact_key }
CleanupDone { result }
```

Phase values should carry the data needed for the next step. After
`DecisionWritten`, resume must not need the original `MachineRemoveRequest` for
pre-cutover steps. After `ServingCommitWritten`, resume should need only the
phase history plus current projection catch-up evidence.

### Entry And Resume Inputs

Use explicit input modes rather than optional bags:

- initial execution has a full `MachineRemoveRequest`,
- resume execution has a `MachineRemoveId` plus projection catch-up evidence,
- phases after `DecisionWritten` carry enough data to continue without the
  initial request.

If resume is requested with no phase history, return a structured error instead
of synthesizing a request. If initial execution sees existing phase history,
`run_phased` owns the phase-read decision and continues from the latest phase.

### Public Surface

The target public surface is one command entry point that runs the phased
machine remove and may return either `Removed` or `CleanupPending`.

The old split helpers can stay temporarily as private helpers if they reduce
risk during migration, but they should not remain as a second public
orchestration model. If tests need a crash boundary, create it by reopening the
phase store and re-running the phased command, not by calling a separate
manual recovery path.

### p2panda Command Phase Store

Slice 036 proved the p2panda-backed phase store in an E2E-local adapter. This
slice should extract that adapter because the second product path now needs it.

The adapter crate should:

- depend on `mvp-commands` and `mvp-p2panda-facts`,
- implement `CommandPhaseStore`,
- read ordered phase history from `/facts/command/<command>/<intent>/phase/>`,
- reject gaps and conflicting candidates as `CommandError`,
- write intent and phase facts with existing p2panda author/session checks,
- preserve the compare-with-latest append guard introduced in Slice 036.

Do not put p2panda storage into `mvp-commands`; command semantics must remain
transport/store independent.

### Compensation

Machine remove compensation should be explicit and conservative. There is no
fake rollback after serving cutover. Pre-cutover compensation may be no-op
where resource-level idempotency is the real cleanup contract. Post-cutover
errors must surface as `CleanupPending` or structured command errors, not as
silent best-effort cleanup.

If implementation reveals that `run_phased` compensation semantics need a
small generic adjustment, make that change in `mvp-commands` with focused
tests and keep it as its own commit.

## Implementation Units

### U1. Reusable p2panda Command Phase Store

Files:

- `MVP/commands-p2panda/Cargo.toml`
- `MVP/commands-p2panda/src/lib.rs`
- `MVP/Cargo.toml`
- `MVP/e2e/Cargo.toml`
- `MVP/e2e/src/environment_branch_promote_rollback_contract.rs`

Work:

- Move the E2E-local `PandaCommandPhaseStore` behavior into a reusable crate.
- Keep the same command fact key helpers from `mvp-commands`.
- Replace the environment E2E's local adapter with the crate adapter.

Tests:

- Unit-test ordered phase reads, gap rejection, conflict rejection,
  idempotent intent writes, idempotent same-payload phase writes, and stale
  expected-previous append rejection.
- Re-run the environment promote/rollback E2E to prove the extraction did not
  change behavior.

### U2. Machine Remove Phased Command

Files:

- `MVP/machine/Cargo.toml`
- `MVP/machine/src/remove.rs`
- `MVP/machine/src/error.rs`
- `MVP/machine/src/lib.rs`

Work:

- Add a `MachineRemovePhasedCommand` using `Command`, `PhasedCommand`, and
  serializable phase values.
- Keep validation, participant RPCs, fact writers, and serving writer seams
  narrow and testable.
- Fold manual recovery into phase-history resume.
- Preserve structured cleanup pending reasons.
- Remove or de-publicize duplicate begin/finalize/recovery APIs when the E2E
  no longer needs them.

Tests:

- Existing validation-before-mutation tests continue to pass.
- A resume after `ServingCommitWritten` does not replay probe, decision,
  removal-started, or prepare.
- Projection catch-up missing/mismatch returns `CleanupPending`.
- Stop unavailability after cutover returns `CleanupPending` with classified
  cause.
- Tombstone and cleanup-done are written in order after stop.
- A phase conflict returns a structured command error rather than falling back
  to manual recovery.

### U3. Machine Remove E2E Migration

Files:

- `MVP/e2e/Cargo.toml`
- `MVP/e2e/src/machine_remove_contract.rs`
- `MVP/e2e/src/main.rs`

Work:

- Use `PandaCommandPhaseStore` from the reusable adapter crate.
- Replace manual `recover_pending_machine_remove_cleanup` flow with reopening
  the p2panda-backed command phase context and re-running the phased command.
- Use a poisoned or resume-only initial input after restart so the test fails
  if the command replays pre-cutover side effects.
- Keep the existing process-role/data-plane checks: remaining traffic works,
  serving projection rebuilds, removed peer is rejected, and cleanup-done is
  recoverable.

Tests:

- `cargo run -p mvp-e2e -- machine-remove-contract`
- Include metrics for command phase history read time and coordinator outage
  resume time if easy to collect without obscuring the test.

### U4. Documentation And Semantic Leverage Report

Files:

- `MVP/slice-037-machine-remove-phased-command.md`
- `MVP/e2e-proof-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/overall-plan.md`

Work:

- Record what changed, what got simpler, and what did not.
- Update "Changed Since Last Slice" with command-phase-store extraction and
  machine remove's migration.
- Record LOC evidence:
  - old `MVP/machine/src/remove.rs` command/recovery surface,
  - new machine remove phased command surface,
  - deleted E2E-local p2panda phase-store LOC,
  - new reusable adapter LOC,
  - E2E harness before/after.
- If raw LOC increases, call it out honestly and explain whether the
  maintenance surface still improved. Do not frame substrate growth as a win
  unless duplicate product code actually disappeared.

## Simplify And Review Cadence

Use smaller commits inside the slice:

1. adapter extraction,
2. machine phased command implementation,
3. E2E migration,
4. simplify pass,
5. review fixes,
6. docs/report.

Run `ce-simplify-code` after the E2E migration and land simplification as a
separate commit. The simplify pass should look specifically for:

- duplicated phase payload structs,
- old and new public command APIs coexisting unnecessarily,
- p2panda adapter helpers that belong in one shared function,
- phases that carry request data they no longer need,
- tests that assert implementation order without asserting product invariants.

Run code review subagents on the substantial implementation diff, focused on:

- correctness of phase/resume behavior,
- tests proving no pre-cutover replay,
- maintainability and semantic leverage,
- p2panda command fact authorization and conflict handling.

Do not run a full review workflow for tiny follow-up fixes.

## Test Plan

Targeted during implementation:

```text
cd MVP && cargo test -p mvp-commands
cd MVP && cargo test -p mvp-commands-p2panda
cd MVP && cargo test -p mvp-machine --all-targets
cd MVP && cargo run -p mvp-e2e -- environment-branch-promote-rollback-contract
cd MVP && cargo run -p mvp-e2e -- machine-remove-contract
```

Before pushing the completed slice:

```text
MVP_E2E_ALL_TIMEOUT=120s just test
```

The PR must remain draft after push.

## Risks

- `CommandPhaseStore` is synchronous while p2panda writes are async. The current
  E2E adapter uses `block_in_place`; extracting that behavior is acceptable for
  this slice, but a future async phase-store interface may be cleaner if more
  production code starts writing command phases directly from async paths.
- Machine remove has a legitimate `CleanupPending` state. Do not force the
  generic runner to treat every non-terminal operational wait as a failure.
- Removing the old manual recovery helpers too aggressively could make the
  migration harder to review. Prefer a small private helper split if needed,
  but keep the public API from carrying two orchestration models forward.
- If the migration only adds `PhasedCommand` on top of the old split API, the
  slice fails its semantic-leverage goal.

## Done Criteria

- Machine remove uses `run_phased` for initial execution and coordinator
  restart resume.
- The p2panda command phase store is reusable outside the environment E2E.
- Machine remove E2E proves no pre-cutover replay after restart.
- Existing machine remove projection/data-plane/removal invariants still pass.
- The old manual recovery public surface is removed or explicitly made private
  and documented as an implementation detail.
- Semantic-leverage numbers are recorded.
- Simplify pass and substantial code review have run, with actionable findings
  addressed.
- Full `just test` passes under the MVP E2E wall-clock budget.
- PR #188 remains draft.
