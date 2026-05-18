---
title: Slice 036 PhasedCommand Primitive
status: completed
created: 2026-05-18
plan: MVP/slice-036-phased-command-primitive-plan.md
---

# Slice 036 PhasedCommand Primitive

## What Shipped

- Added `mvp-commands` as the first opt-in command orchestration primitive.
- Implemented `run_phased` with explicit phase steps, ordered command
  phase-store records, resume from latest phase, conditional phase append,
  best-effort reverse compensation, and structured phase-conflict errors.
- Migrated environment promote and rollback onto the primitive while keeping
  branch as a plain command.
- Updated `environment-branch-promote-rollback-contract` so promote and rollback
  both pause after serving commit and resume from p2panda-backed command phase
  facts without rewriting decision or serving-commit facts.

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
- `CommandContext` still depends only on the `CommandPhaseStore` trait.
  p2panda-backed command phases are proven in the E2E harness without adding a
  p2panda dependency to `mvp-commands`.

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

After review fixes, the final focused E2E reported `elapsed_ms: 359` with the
same projection rebuild and serving-alive evidence. The runner tests also cover
full-history resumed failure compensation, compensation after phase-write
failure, stale concurrent phase append rejection, p2panda phase conflict
rejection, and compensation-error suppression preserving the original caller
error.

## Semantic Leverage

LOC is not the only target, but it is a useful warning light:

- `mvp-commands` is 1,065 lines after the first lift and hardening pass.
- `MVP/environment/src/command.rs` grew from 608 lines before the slice to 871
  lines because it now carries explicit promote and rollback phase enums.
- The environment unit tests grew from 1,117 to 1,299 lines to cover phased
  resume for both commands.
- The process-role E2E grew from 632 lines to 976 lines because it now includes
  a p2panda-backed command phase store and reopens that store before resuming
  promote and rollback with poisoned requests.

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
- The p2panda command phase store is E2E-local. Extract it into a reusable
  adapter only when a second command path needs the same durable phase store.
