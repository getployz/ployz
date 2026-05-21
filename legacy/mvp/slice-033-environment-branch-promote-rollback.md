---
title: Slice 033 Environment Branch Promote Rollback Report
status: implemented
created: 2026-05-18
origin:
  - MVP/slice-033-environment-branch-promote-rollback-plan.md
  - MVP/overall-plan.md
  - MVP/primitive-decisions.md
  - MVP/e2e-proof-plan.md
  - VISION.md
---

# Slice 033 Environment Branch Promote Rollback Report

## Result

The MVP now has an environment branch/promote/rollback product proof.

The slice adds `mvp-environment` for typed environment facts and commands,
`mvp-environment-p2panda` for p2panda fact writes, and
`environment-branch-promote-rollback-contract` in the E2E suite.

The proof covers:

- production environment head seeded from p2panda-backed facts,
- branch volume lineage from `prod` to `pr-123`,
- promotion as a decision fact before serving cutover,
- projection catch-up before promote finalization,
- rollback as a new forward head using the previous head's volume refs,
- serving process survival after the command adapter is dropped,
- projection SQLite deletion and rebuild recovering the rolled-back state.

## Shape

Environment facts stay small:

- heads carry environment epoch, serving commit id, previous-head reference,
  volume refs, and optional source branch id;
- branch facts carry source head evidence, forked volume refs, route refs, and
  visible nodes at decision time;
- promote and rollback decision facts record operator intent before serving
  changes.

Serving payloads remain owned by `mvp-routing`. Environment commands use
`ServingFactWriter` and `ProjectionCatchUp`; they do not construct gateway/DNS
projection payloads directly.

## Implementation Notes

`mvp-commands` remains deferred. This slice added explicit begin/finalize
boundaries, but it did not add a third command with enough persisted phase and
resume bookkeeping to justify a generic command runner.

Branch revalidates the source head after participant volume-fork work because
the command awaits external side effects before writing durable branch facts.
Promote and rollback gather all decision inputs, then re-read the relevant
heads and compare exact fact key/content hash before their first mutation. That
keeps the command-entry conflict boundary explicit without adding a separate
workflow primitive.

## Leverage

This slice is a positive semantic-leverage signal. A new product primitive was
added mostly by composing existing pieces:

- p2panda facts,
- routing-owned serving commits,
- projection catch-up,
- process-role serving,
- typed visible-node evidence,
- explicit command entry conflict checks.

The environment-specific p2panda adapter is small and follows the same backend
shape as routing, deploy, and machine adapters instead of introducing another
store path.

Rough LOC accounting:

- `mvp-environment` plus `mvp-environment-p2panda`: about 2,000 LOC.
- New E2E proof: one scenario file plus registration.
- Honest old-codebase baseline for this exact branch/promote/rollback surface
  was not identified, so this report does not claim a direct LOC win against an
  old file.

## Verification

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-environment --all-targets`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-environment-p2panda --all-targets`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-e2e --all-targets`
- `cargo clippy --manifest-path MVP/Cargo.toml --workspace --all-targets -- -D warnings`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- environment-branch-promote-rollback-contract`
- `MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all`
  completed under the time budget.
