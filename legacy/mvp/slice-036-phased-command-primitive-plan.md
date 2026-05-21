---
title: Slice 036 PhasedCommand Primitive Plan
status: active
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/phased-command.md
  - MVP/slice-035-p2panda-authz-fact-authority.md
external:
  - https://docs.restate.dev/tour/workflows
  - https://docs.restate.dev/develop/java/journaling-results/
  - https://docs.dbos.dev/architecture
  - https://docs.rs/restate-sdk/latest/restate_sdk/
---

# Slice 036 PhasedCommand Primitive Plan

## Problem Frame

The next product proof should pay down business-code choreography, not add
another generic substrate layer. The repeated shape is now visible in enough
places to plan the `mvp-commands` lift:

- `mvp-deploy` owns phase transitions, serving commit boundaries, pre-commit
  cleanup, recovery from decision facts, and post-commit cleanup resume.
- `mvp-machine` owns a begin/finalize split, durable remove decision,
  projection-gated cleanup, cleanup-done proof, and recovery from command
  facts.
- `mvp-environment` promote/rollback own the same begin/finalize split around a
  serving commit and projection catch-up.
- `mvp-volume` has the same side-effect-before-commit shape for snapshot,
  receive, lease fencing, and ownership commit, but it does not yet have durable
  phase facts or resume.

That is enough repetition to plan a small command substrate. It is not enough
evidence for a workflow engine, replay system, scheduler, or planner/executor
split.

The proof target is narrow: introduce a tiny `mvp-commands` crate and migrate
one product command path to it. The migrated command must become easier to read
because business steps are visible as phase arms while fact read/write,
resume-from-phase, and best-effort compensation bookkeeping move into the
substrate.

## Crate Scout

Checked on 2026-05-18:

- Restate and `restate-sdk` are strong external references for durable
  execution and Rust support, but their model is intentionally larger than this
  slice. Restate uses a server/runtime plus journaled durable steps and replay
  of operations. Ployz explicitly wants local fact-backed phase facts and no
  activity replay magic.
- DBOS is a useful reference for checkpoint/recover semantics, but current DBOS
  language/runtime support is centered on Python, TypeScript, Go, and Java docs
  and introduces a Postgres-backed durable execution model. That is not the MVP
  substrate.
- Temporal/Cadence-style workflow replay remains the wrong semantic shape for
  Ployz commands. The reader must see exactly which phase runs next; function
  bodies should not secretly replay with recorded activity results.
- Existing MVP dependencies already include `async-trait` transitively and
  `trait-variant` through p2panda, but the first pass should avoid adding an
  async trait dependency if boxed futures keep the public surface clear enough.
- The non-RC iroh path is acceptable. This slice should not change iroh at all,
  and future command work should keep command traits independent from iroh,
  p2panda, or transport crate versions.
- `cargo search` shows `restate-sdk` 0.10.0 exists, `async-trait` 0.1.89 is
  current, and `trait-variant` 0.1.2 is available. `cargo info` shows
  `restate-sdk` brings a durable-execution SDK surface and HTTP/runtime
  dependencies; adopting it would move Ployz toward an external workflow
  runtime instead of the fact-backed command primitive this slice is testing.
  `async-trait` and `trait-variant` stay as simplify-pass options, not default
  dependencies.

Decision: build the smallest Ployz-owned command primitive because existing
workflow crates solve a broader problem with replay/server semantics we do not
want. Keep the design close enough to the external systems' proven ideas:
durable step boundaries, explicit status, recovery from the last recorded
phase, and visible compensation.

## Scope

In scope:

- Add `mvp-commands` to the isolated `MVP/` workspace.
- Define the minimal command vocabulary:
  - `CommandName`
  - `IntentId`
  - `PhaseName`
  - `Command`
  - `Phase`
  - `PhasedCommand`
  - `PhaseTransition`
  - `CommandContext`
  - `run_phased`
- Back `CommandContext` with narrow traits for phase reads/writes and phase
  data. Do not add bus request/request-many, fact pin, or advisory lease
  methods until a migrated command actually exercises them.
- Store phase facts as ordinary MVP facts. The primitive must not create a
  second persistence model.
- Migrate exactly one product command path as the proof.
- Add E2E coverage proving resume and compensation behavior through the migrated
  command path.
- Report semantic leverage: before/after command LOC, phase-bookkeeping LOC,
  business-step LOC, and test LOC.

Out of scope:

- No deploy rewrite in this slice unless chosen as the single migrated proof.
- No generic scheduler, queue, workflow server, timers, cancellation engine, or
  background task runner.
- No activity replay, deterministic replay, or hidden journaled return values.
- No macros.
- No command registry in the daemon/bus.
- No migration outside `MVP/`.
- No iroh dependency changes, RC or otherwise.
- No replacement for p2panda facts, PloyzBus, advisory leases, or projections.

## Requirements Traceability

- `VISION.md` says operations must be command-shaped, explicit, retryable, and
  honest about partial progress. `run_phased` must therefore return structured
  command results instead of hiding retries or background convergence.
- `MVP/design-notes/phased-command.md` sets the trigger: lift the phase pattern
  only when three or more commands repeat it. That threshold is now met by
  deploy, machine remove, environment promote/rollback, and volume transfer.
- `MVP/e2e-proof-plan.md` E2E-9 requires semantic-leverage evidence. This
  slice must report command LOC and phase-bookkeeping shape, not just add a
  new crate.
- `MVP/overall-plan.md` keeps scale and reliability as mandatory proof work,
  but it also names Slice 036 as the first command-semantic-leverage proof.
  This slice should not close the remaining E2E-8 randomized-failure and
  packet-delay/drop gaps; those stay queued as a separate reliability harness
  slice after the command primitive has a real product migration.
- The user constraint remains hard: all new code and docs stay under `MVP/`,
  and the GitHub PR stays draft.

## Candidate Migration

Prefer migrating `mvp-environment` promote/rollback first.

Reasons:

- It has the durable begin/finalize shape that motivated this primitive.
- The serving commit/projection catch-up boundary is real product behavior.
- It is smaller and less risky than deploy.
- It already has two similar command paths, so the migration can prove reuse
  without broadening the slice.
- It avoids hiding deploy's more subtle irreversible-phase and cleanup semantics
  before the command primitive has earned trust.

Fallback: migrate volume transfer only if environment proves too coupled. Volume
is useful for side-effect/commit readability, but it lacks durable phase facts
today, so it is a weaker proof of resume.

Do not migrate ACME first. ACME claim/present/clear are valuable product
canaries, but they are currently plain commands and should stay plain until
certificate issuance grows enough phase boundaries.

## Existing Patterns To Follow

- `MVP/environment/src/command.rs` is the proof target. Its promote and
  rollback paths already separate `begin` from `finalize`, write decision facts
  before serving commits, and return pending projection-catch-up results.
- `MVP/machine/src/remove.rs` shows the heavier recovery shape this primitive
  should eventually absorb, but it should not be migrated in this slice.
- `MVP/deploy/src/coordinator.rs` shows commit-before-drain invariants and
  cleanup recovery. Use it as a reviewer reference for failure semantics, not
  as the first migration target.
- `MVP/volume/src/command.rs` shows the side-effect-before-commit pattern that
  will benefit later, but its durable phase facts are not present yet.
- `MVP/environment-p2panda/src/lib.rs` is the current reusable environment fact
  writer. The migration should reuse that boundary instead of teaching
  `mvp-commands` about p2panda.

## Design

### Command Facts

Command phase state is just fact data:

```text
/facts/command/<command_name>/<intent_id>/intent
/facts/command/<command_name>/<intent_id>/phase/<phase_index>
/facts/command/<command_name>/<intent_id>/data/<name>
/facts/command/<command_name>/<intent_id>/done
```

The exact key helper names belong in `mvp-commands`. The fact payloads must
carry:

- command name,
- intent id,
- phase name or serialized phase value,
- previous phase hash where applicable,
- author principal,
- written-at timestamp when the caller has one,
- visible nodes at decision time when the command result needs to surface it.

Reducers should pick the current phase deterministically from candidate facts.
If candidates conflict, use the existing conflict-as-candidate model and expose
structured conflict status to the caller. Do not ask the operator to pick.

### Traits

Keep the first version explicit and boring:

```rust
trait Command {
    type Output: Send;
    type Error: Send;

    fn name(&self) -> CommandName;
    fn intent_id(&self) -> IntentId;
    fn intent_fact(&self) -> CommandFact;
}

trait Phase:
    serde::Serialize
    + serde::de::DeserializeOwned
    + Clone
    + std::fmt::Debug
    + Eq
    + std::hash::Hash
    + Send
    + Sync
    + 'static
{
}

trait PhasedCommand: Command {
    type Phase: Phase;

    fn initial_phase(&self) -> Self::Phase;
    fn step<'a>(
        &'a self,
        cx: &'a CommandContext,
        phase: Self::Phase,
    ) -> Pin<Box<dyn Future<Output = Result<PhaseTransition<Self::Phase, Self::Output>, Self::Error>> + Send + 'a>>;
    fn compensate<'a>(
        &'a self,
        cx: &'a CommandContext,
        phase: Self::Phase,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send + 'a>>;
}
```

If boxed futures become noisy in the migrated command, add `async-trait` in a
separate simplify commit. Do not start there.

### Runner Semantics

`run_phased(cx, cmd)`:

1. reads the latest command phase fact,
2. defaults to `cmd.initial_phase()` if none exists,
3. calls exactly one explicit `step` for the current phase,
4. writes the next phase fact after `Continue(next)`,
5. returns on `Done(output)`,
6. on error, compensates already committed phases in reverse order and returns
   the original error.

The runner does not compensate the failing phase. If a step performs a side
effect before returning an error, the step owns its own cleanup.

Crash mid-step means that same step may run again on resume because no new
phase fact was written. Step implementations must therefore be idempotent up to
the next phase fact write.

### CommandContext

`CommandContext` is a thin facade, not a service locator. It should expose only
what the migrated command needs in this slice:

- `read_phase`
- `write_phase`
- `read_phase_data`
- `write_phase_data`
- `write_intent`

Each method should delegate to narrow traits so product crates can test command
logic without p2panda or iroh. Avoid importing p2panda types into
`mvp-commands`. Later commands may add request, request-many, lease, and pin
helpers when they migrate, but unused context methods are not allowed in the
first lift.

### Migration Shape

For environment promote/rollback, expected phases are roughly:

```text
Start
DecisionWritten
ServingCommitWritten
ProjectionObserved
HeadWritten
Done
```

The command-specific phase enum lives in `mvp-environment`, not
`mvp-commands`.

Business code after migration should be one `match phase` with visible steps:

- read/validate expected heads,
- write decision,
- write serving commit,
- wait/accept projection catch-up input,
- write new environment head,
- return complete.

If projection catch-up is absent or mismatched, return a pending result without
pretending the command failed. This preserves the current foreground audience.

## Implementation Units

### Unit 1: `mvp-commands` Core

Files:

- `MVP/Cargo.toml`
- `MVP/commands/Cargo.toml`
- `MVP/commands/src/lib.rs`
- `MVP/commands/src/tests.rs`

Build the smallest crate that can run one migrated command:

- identity newtypes for command name, intent id, and phase index/name,
- `Command`, `Phase`, `PhasedCommand`, and `PhaseTransition`,
- a `CommandContext` backed by narrow traits for phase read/write and phase
  data read/write,
- an in-memory test store for command facts,
- `run_phased` with no replay and no hidden retry loop.

Test scenarios:

- fresh command starts at `initial_phase`,
- persisted phase resumes from that phase instead of starting over,
- `Continue(next)` writes exactly one next-phase fact before the next step,
- `Done(output)` returns without writing another phase,
- compensation walks committed phases in reverse,
- compensation does not run for the failing phase,
- compensation failure does not hide the original command error,
- conflicting phase candidates return a structured command error instead of
  picking silently.

### Unit 2: Environment Promote/Rollback Migration

Files:

- `MVP/environment/Cargo.toml`
- `MVP/environment/src/command.rs`
- `MVP/environment/src/error.rs`
- `MVP/environment/src/tests.rs`

Migrate promote and rollback onto `run_phased` while preserving their public
command behavior. Keep the environment-specific phase enum in
`mvp-environment`; do not put product phases in `mvp-commands`.

Test scenarios:

- promote writes decision before serving commit,
- promote returns pending when projection catch-up is missing,
- promote completes from a persisted serving-commit phase without writing a
  second decision or serving commit,
- promote completes from a persisted head-written phase without replaying side
  effects,
- rollback writes a forward head using previous volume refs,
- rollback resumes from serving-commit phase and preserves pending catch-up
  semantics,
- stale expected epoch still fails before any mutation,
- serving write conflict still maps to the existing structured environment
  error.

### Unit 3: Environment E2E Proof

Files:

- `MVP/e2e/src/environment_branch_promote_rollback_contract.rs`
- `MVP/e2e/src/main.rs`

Update the existing environment product canary rather than adding a second
nearly identical scenario. The E2E should prove the migrated command path over
the p2panda-backed environment writer and serving process boundary.

Test scenarios:

- fresh branch/promote/rollback flow still passes,
- promote recovery from `DecisionWritten` resumes without duplicating the
  decision fact,
- promote recovery from `ServingCommitWritten` waits for or accepts projection
  catch-up,
- rollback recovery from `ServingCommitWritten` reaches a forward head,
- serving process still answers last-good state while the command adapter is
  absent,
- command result keeps visible nodes at decision time.

### Unit 4: Documentation And Semantic-Leverage Accounting

Files:

- `MVP/e2e-proof-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/overall-plan.md`
- `MVP/slice-036-phased-command-primitive.md`

Record the decision and the evidence:

- what command bookkeeping moved into `mvp-commands`,
- what product logic stayed in `mvp-environment`,
- before/after LOC for promote/rollback command code and E2E harness code,
- remaining hand-rolled phase command candidates,
- why ACME remains a plain command.

### Unit 5: Simplify Pass

Files:

- Same files changed by Units 1-3.

After the first green implementation commit, run a dedicated simplify pass and
land it separately. Delete unused context methods, reduce duplicated phase
helpers, and only add `async-trait` or `trait-variant` if they make the real
migrated command easier to read.

## E2E Proof

Add or update E2Es to prove:

- fresh promote/rollback completes through `run_phased`,
- recovery from `DecisionWritten` resumes without writing a second decision,
- recovery from `ServingCommitWritten` waits for/accepts projection catch-up,
- recovery after `HeadWritten` returns complete without replaying side effects,
- compensation runs in reverse order for a synthetic failing command,
- failing phase compensation does not run for the failing phase itself,
- command result includes visible nodes at decision time.

Keep E2E time-budgeted under `cargo run -p mvp-e2e -- all`.

The remaining E2E-8 scale-reliability items are deliberately not mixed into
this slice. The next reliability slice should still add 100 deploy attempts
with deterministic node failures and bus delay/drop simulation. Pulling that
into this command-primitive migration would make it hard to tell whether
failures came from the new substrate or the stress harness.

## Simplify Pass Targets

Run a dedicated simplify pass after the first green implementation commit.
Specific targets:

- boxed-future noise in command traits,
- duplicate promote/rollback phase code,
- command fact key/payload helper boilerplate,
- any "generic framework" code not exercised by the migrated command,
- extra accessors on phase structs that only force clones inside a module.

The simplify pass should be its own commit.

## Review Gates

Before shipping the slice:

- Run subagent code review focused on correctness, maintainability, and testing.
- Ask reviewers to treat `mvp-commands` as production substrate, not a
  placeholder.
- Require reviewers to check for hidden replay semantics, daemon/bus registry
  creep, command context overreach, and wildcard enum matches.
- Address real findings before final response.

Do not run a heavyweight review on plan-only edits or tiny mechanical fixes.
Run the subagent review after the implementation and simplify commits exist.

## Risks

- The primitive could become a workflow framework. Guardrail: no registry,
  timers, background scheduler, macros, or replay model in this slice.
- `CommandContext` could become a service locator. Guardrail: include only
  methods exercised by the migrated environment command and tests.
- Environment migration could obscure the product behavior. Guardrail: the
  phase enum and `match phase` business logic stay in `mvp-environment`.
- Boxed futures could make the migrated command noisier than the explicit
  begin/finalize code. Guardrail: simplify pass may add `async-trait`, but only
  after the first implementation proves the public surface.
- E2E runtime could grow. Guardrail: update the existing environment scenario
  and keep `MVP_E2E_ALL_TIMEOUT` at the existing budget unless measurements
  prove a needed adjustment.

## Verification

Required before commit/push:

```text
cd MVP && cargo fmt --all -- --check
cd MVP && cargo check --workspace --all-targets
cd MVP && cargo clippy --workspace --all-targets -- -D warnings
cd MVP && cargo test -p mvp-commands
cd MVP && cargo test -p mvp-environment
cd MVP && cargo run -p mvp-e2e -- environment-branch-promote-rollback-contract
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
cd MVP && just test
```

If the migrated command is not environment, replace package/scenario names with
the chosen proof target and document why the plan changed in the slice report.

## Success Criteria

- One real product command path uses `mvp-commands`.
- The command's business logic is easier to read than the pre-slice version.
- Resume behavior is fact-backed and E2E-proven.
- Compensation behavior is explicit and E2E-proven.
- Simple ACME commands remain plain commands.
- Deploy is not made more abstract unless the migration target changes
  deliberately.
- No transport, iroh, p2panda, or daemon dependency leaks into
  `mvp-commands`.
- The slice report includes semantic-leverage accounting and names any
  remaining manual phase bookkeeping that should be targeted next.
