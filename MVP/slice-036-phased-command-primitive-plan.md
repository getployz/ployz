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
- Back `CommandContext` with narrow traits for phase reads/writes, phase data,
  bus request/request-many, fact pin/local durability, and advisory leases.
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
- `request`
- `request_many`
- `acquire_lease`
- `pin_facts` or local durable write acknowledgement

Each method should delegate to narrow traits so product crates can test command
logic without p2panda or iroh. Avoid importing p2panda types into
`mvp-commands`.

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

## Verification

Required before commit/push:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p mvp-commands
cargo test -p mvp-environment
cargo run -p mvp-e2e -- <new-or-updated-environment-command-scenario>
just test
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
