# PhasedCommand Design Hook

This note is a trigger for a future slice, not an implementation plan for the
current ACME/deploy work.

## Why This Exists

The bus, authority, fact, projection, and actor primitives remove most of the
substrate glue from business code. The recurring pattern still visible in ACME
and deploy is phase bookkeeping: read the latest phase/intention fact, decide
where to resume, write a new phase fact before each irreversible side effect,
and walk back committed phases on failure.

When three or more commands have this shape, lift it into a small
`mvp-commands` crate. Until then, keep the phase logic explicit in each command
so the primitive is grounded in real repetition.

## Proposed Types

`Command` is the base application-level operation:

- `Send + Sync + 'static`
- associated `Output: Send`
- associated `Error: Send + From<BusError> + From<PinError>`
- `name() -> CommandName`
- `intent_fact(intent_id: IntentId) -> Fact`

`CommandContext` carries the application substrate needed by command bodies:

- bus session
- fact authorizer
- lease access
- pin client
- scoped phase fact reads/writes
- `read_phase<P: Phase>(&self, cmd: &impl Command) -> Result<Option<P>, CommandError>`
- `write_phase<P: Phase>(&self, cmd: &impl Command, phase: P) -> Result<(), CommandError>`
- `read_phase_data<T: DeserializeOwned>(&self, cmd: &impl Command, key: &str)`
- `pin_facts<const N: usize>(&self, facts: [Fact; N]) -> Result<PinCommit, PinError>`
- `acquire_lease(&self, key: LeaseKey, ttl: LeaseTtl) -> Result<Lease, LeaseError>`
- typed `request` and `request_many` helpers over PloyzBus

`Phase` is the persisted phase marker:

- `Serialize + DeserializeOwned + Clone + Debug + Eq + Hash + Send + Sync + 'static`

`PhasedCommand` extends `Command`:

- associated `Phase: Phase`
- `initial_phase() -> Self::Phase`
- `step(&self, cx: &CommandContext, phase: Self::Phase) -> Result<PhaseTransition<Self::Phase, Self::Output>, Self::Error>`
- `compensate(&self, cx: &CommandContext, phase: Self::Phase) -> Result<(), Self::Error>`

`PhaseTransition` has two variants:

- `Continue(next_phase)`
- `Done(output)`

## Runner Semantics

`run_phased(cx, cmd)` reads the latest phase fact, defaults to
`cmd.initial_phase()`, then loops:

1. call `cmd.step(cx, current.clone())`,
2. on `Continue(next)`, write the next phase fact, remember `current` as
   committed, and continue,
3. on `Done(output)`, return the output,
4. on error, compensate committed phases in reverse order and return the
   original error.

Compensation is best-effort and never hides the original command error.
Compensation does not run for the failing phase because that phase did not
write its phase fact. Cleanup inside a failing step remains the command
author's responsibility.

## Non-Negotiables

- No activity-replay magic. This is not Temporal, Cadence, Restate, or DBOS.
  A resumed command runs exactly one explicit phase step; it does not replay a
  function body from the top with secretly recorded activity results.
- Compensation is a readable `match` inside `compensate`, next to the step
  logic it compensates. No registered closures or attribute macros.
- No planner/executor split. Multi-step data such as deploy plans, transfer
  snapshots, and capacity decisions are just command facts read by later
  phases.
- Keep `mvp-commands` separate from `mvp-bus`. The bus is messaging substrate;
  commands are application orchestration.
- Keep the trait opt-in. Commands with fewer than three phase boundaries and no
  meaningful compensation should stay plain `Command`.

## Trigger

After slice 014 and the next deploy/ACME/membership slices, count commands with
a phase enum, transition methods, and resume-from-phase logic. If three or more
commands repeat the pattern, plan and ship the `mvp-commands` slice.
