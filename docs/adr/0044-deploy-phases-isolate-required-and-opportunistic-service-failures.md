# Deploy phases isolate required and opportunistic service failures

## Status

Accepted target contract; implementation tracked by
[#894](https://github.com/getployz/ployz/issues/894)

## Context

Ployz is undergoing a substantial deploy rewrite and the current runtime admits
one Service per Namespace. Ployz Cloud must not block its Saved/Applied state
model on those internals, but it needs a precise future runtime boundary for one
cohesive Environment deployment with dependency ordering and per-Service
outcomes.

A single coarse failure cannot express that useful Services applied while a
broken sibling retained its incumbent or prior absence. Making each Service a
separate Cloud deployment would expose runtime implementation details and lose
one understandable Environment history.

## Decision

The desired Cloud-facing runtime API accepts one complete Service target plus
ordered dependency phases. Every affected Service action, including removal,
is marked required or opportunistic for that attempt.

The runtime executes all eligible Services in a phase independently. A new
candidate must pass its normal creation health gate before promotion regardless
of requirement. Successful Services in a phase stay promoted even if a sibling
fails. Failed update or removal retains its incumbent; a failed new Service
retains prior absence.

A required failure records the current phase's results and skips every later
phase. An opportunistic failure is recorded and does not cancel later phases.
Unchanged Services count as satisfied. The runtime does not create an automatic
retry loop; a later complete request reconciles from durable intent and host
reality.

Terminal evidence contains ordered per-phase and per-Service results that
distinguish applied, removed, unchanged, failed, skipped, and interrupted work.
Confirmed successes remain evidence when the overall request fails. An
ambiguous commit is interrupted; the runtime does not guess whether it landed
or continue into later phases.

This ADR fixes observable request, result, and failure semantics. It does not
prescribe how the rewritten runtime maps them onto Namespace rows, controllers,
commits, or internal planners. Until #894 lands, Cloud owns contract fixtures
and keeps one localized runtime-adapter TODO rather than claiming the current
Rust API supports the behavior.

Required and opportunistic are attempt policy, not health policy. Ployz does
not know Cloud Working, Saved, Applied, pending, or destructive-confirmation
state. Cloud compiles those product decisions into this boundary as described
by
[Ployz Cloud ADR 0002](https://github.com/getployz/ployz-dashboard/blob/main/docs/adr/0002-saved-state-is-deployable-intent.md).

## Consequences

- Cloud can implement and test its state seam without blocking on the Rust
  rewrite.
- One broken opportunistic Service cannot block an intended Git update.
- A required dependency failure prevents later dependants from starting while
  successful peers remain live.
- Cloud can advance Applied State from confirmed per-Service results and leave
  failed or skipped Saved work pending.
- Rust `CONTEXT.md` continues to describe only current implementation. Issue
  #894 owns the future wire types, execution mapping, and glossary update.
