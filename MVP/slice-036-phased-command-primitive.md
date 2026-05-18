---
title: Slice 036 PhasedCommand Primitive
status: completed
created: 2026-05-18
plan: MVP/slice-036-phased-command-primitive-plan.md
---

# Slice 036 PhasedCommand Primitive

## What Shipped

- Added `mvp-commands` as the first opt-in command orchestration primitive.
- Implemented `run_phased` with explicit phase steps, persisted phase facts,
  resume from latest phase, best-effort reverse compensation, and structured
  phase-conflict errors.
- Migrated environment promote and rollback onto the primitive while keeping
  branch as a plain command.
- Updated `environment-branch-promote-rollback-contract` so promote and rollback
  both pause after serving commit and resume without rewriting decision or
  serving-commit facts.

The important semantic boundary stayed intact: the command runner only owns
phase bookkeeping. Product phases and business decisions stay in
`mvp-environment`.

## Deliberate Simplifications

- No `PhaseName` type shipped. Serialized phase values and monotonic phase
  indexes are enough for the first migrated command.
- `Phase` does not require `Hash`; the runner never hashes phase values.
- No request/request-many, lease, pin, or phase-data helpers were added to
  `CommandContext`. They should be added only when the next migrated command
  needs them.
- No `async-trait` dependency was added. Boxed future aliases keep the current
  surface readable enough.

## Proof

Targeted verification run:

```text
cargo test --manifest-path MVP/Cargo.toml -p mvp-commands --all-targets
cargo test --manifest-path MVP/Cargo.toml -p mvp-environment --all-targets
cargo check --manifest-path MVP/Cargo.toml -p mvp-e2e --all-targets
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- environment-branch-promote-rollback-contract
```

The E2E reported:

```text
PASS environment-branch-promote-rollback-contract
elapsed_ms: 297
projection_rebuilds: 3
serving_alive_without_command_adapter: true
```

## Semantic Leverage

LOC is not the only target, but it is a useful warning light:

- `mvp-commands` is 659 lines after the first lift.
- `MVP/environment/src/command.rs` grew from 608 lines before the slice to 914
  lines because it now carries explicit promote and rollback phase enums.
- The environment unit tests grew from 1,117 to 1,277 lines to cover phased
  resume for both commands.
- The process-role E2E stayed essentially flat: 632 lines before the slice and
  637 lines after.

The sidecar LOC investigation found a real deploy-shaped win but not yet a
total MVP LOC win:

- Old deploy handler baseline: 4,558 physical lines.
- MVP deploy plus deploy-p2panda: about 3,354 SLOC.
- Current MVP substrate is large: about 62,808 Rust SLOC total under `MVP/`,
  with about 25,859 SLOC in selected shared foundation crates.

The conclusion is yellow-green: complexity is moving into reusable primitives,
but this only pays off if the next commands reuse `mvp-commands`, bus, facts,
projection, and leases without adding new local recovery machinery.

## Follow-Up

- Consider machine remove or volume transfer as the next `mvp-commands`
  migration only after a simplify pass decides whether promote/rollback phase
  duplication should be extracted locally.
- E2E-8 reliability gaps remain: 100 deploy attempts with deterministic node
  failures, plus bus delay/drop simulation.
- `mvp-commands` still needs a p2panda-backed command phase store before it is
  more than an in-memory command primitive.
