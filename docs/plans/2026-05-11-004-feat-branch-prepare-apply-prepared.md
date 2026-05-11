---
title: "feat: Branch prepare and apply-prepared"
type: feat
status: active
date: 2026-05-11
origin:
  - VISION.md
  - docs/plans/2026-05-11-001-feat-durable-prepared-deploys.md
  - docs/plans/2026-05-11-003-feat-branch-command-plan-compiler.md
---

# feat: Branch prepare and apply-prepared

## Summary

Make `branch` usable as a durable approval primitive by adding a prepare path
that compiles the branch request once, stores the resulting deploy manifest,
preview evidence, and baseline in the existing prepared deploy record, and then
applies that prepared id later.

This closes the important gap left by `branch preview` plus `branch apply`:
those are separate compiles and therefore cannot prove that apply used the same
source/target state the operator previewed. `branch prepare` becomes the safe
approval handoff. Direct `branch apply` remains a convenience that compiles at
invocation time.

## Problem Frame

The branch compiler turns a source namespace and target namespace into a deploy
manifest with service branch and volume clone source evidence. Deploy already
has durable prepared records with baseline enforcement. The missing primitive is
connecting those two surfaces so a user, cloud workflow, or agent can preview a
branch exactly once and later apply that exact compiled plan by id.

The core should not add a parallel "prepared branch" model. Branch is a
compiler over deploy, and deploy owns mutation, baseline validation, and
prepared intent records.

## Requirements

- Add a `branch prepare SOURCE TARGET` command that accepts the same branch
  source-mode flags as preview/apply.
- `branch prepare` compiles the branch manifest once and delegates to the
  existing deploy prepare path.
- The prepared record stores the compiled manifest, including service source
  revision pins and volume clone source fingerprints.
- Add a `branch apply-prepared PREPARED_DEPLOY_ID` command that applies the
  existing prepared deploy record by id.
- Do not introduce a branch-specific execution engine or durable record type.
- Preserve existing `branch render-manifest`, `branch preview`, and direct
  `branch apply` behavior.
- Make the API/CLI semantics explicit: only prepare/apply-prepared is the
  preview-to-apply-safe branch approval flow.
- Route branch prepare/apply-prepared through the same shared deploy lane as
  deploy prepare/apply-prepared.
- Add e2e coverage so the real branch clone scenario exercises the durable
  prepare/apply-prepared flow rather than only direct apply.

## Assumptions

- Existing prepared deploy records are sufficient for branch because they store
  manifest JSON, preview evidence, baseline, namespace, and lifecycle state.
- `branch apply-prepared` can build a `DeployApplyPreparedRequest` internally
  instead of adding a new daemon request variant. If the daemon API later needs
  branch-specific telemetry, it can be added without changing the durable model.
- Direct `branch apply` remains useful for fast operator flows, but it does not
  claim to apply an earlier preview.
- Backwards compatibility is not a constraint for enum additions or CLI shape.

## Scope

In scope:

- API enum support for `BranchNamespaceMode::Prepare`.
- CLI parser and request builder support for `branch prepare` and
  `branch apply-prepared`.
- Daemon branch handler support for prepare via deploy prepare delegation.
- Request routing and metrics updates if a branch-specific apply-prepared
  request is needed during implementation.
- E2E scenario coverage for branch prepare/apply-prepared.
- Focused unit tests for API serde, CLI parsing, request building, daemon
  delegation, and branch prepared record source evidence.

Out of scope:

- New prepared branch store records.
- Portal attach, service move, cross-machine volume copy, data masking hooks, or
  PR metadata records.
- Removing or warning on direct `branch apply`.
- Cloud dashboard changes.

## Key Technical Decisions

1. Branch prepare compiles once, then stores deploy intent.
   The safety property comes from the stored compiled manifest plus deploy
   baseline, not from recompiling the branch request during apply.

2. Apply-prepared stays deploy-owned.
   A prepared id is already a deploy identity. The branch command can expose a
   branch-flavored CLI alias while sending the existing deploy apply-prepared
   request.

3. Branch preview remains non-durable.
   It is useful for quick inspection, but callers that need approval semantics
   must use prepare. This avoids pretending the daemon can correlate an
   arbitrary old preview with a later apply call.

4. Keep branch as a deploy compiler.
   Branch should not know about participant RPCs, runtime startup, clone RPCs,
   or commit mechanics. It should compile and delegate.

## Existing Patterns

- `crates/ployzd/src/daemon/handlers/deploy.rs` already compiles branch
  requests and delegates render/preview/apply to deploy handlers.
- `crates/ployzd/src/daemon/handlers/deploy.rs` already implements
  `handle_deploy_prepare` and `handle_deploy_apply_prepared`.
- `crates/ployz-orchestrator/src/deploy/mod.rs` stores prepared records through
  `prepare` and validates stored manifest hashes through
  `validated_prepared_manifest`.
- `crates/ployz-orchestrator/src/deploy/execute.rs` applies prepared deploys
  with the stored baseline before participant mutation.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`
  supports validating final source truth before mutation.

## Implementation Units

### U1. Add branch prepare API mode

Files:

- Modify: `crates/ployz-api/src/deploy.rs`
- Test: `crates/ployz-api/src/deploy.rs`

Approach:

- Add `Prepare` to `BranchNamespaceMode`.
- Keep snake_case JSON behavior.
- Ensure `BranchNamespaceRequest` serde round-trips the new mode.

Test scenarios:

- `BranchNamespaceRequest` round-trips with `mode: prepare`.
- Existing branch modes still round-trip explicitly.

### U2. Wire daemon branch prepare through deploy prepare

Files:

- Modify: `crates/ployzd/src/daemon/handlers/deploy.rs`
- Test: `crates/ployzd/src/daemon/handlers/deploy.rs`

Approach:

- Extend `handle_branch_namespace` with a `Prepare` arm.
- Encode the compiled branch manifest and delegate to `handle_deploy_prepare`.
- Add coverage that the stored prepared manifest includes the branch target
  namespace and source intent evidence.

Test scenarios:

- `handle_branch_namespace` in prepare mode returns a `DeployPrepare` payload.
- The prepared manifest namespace is the branch target namespace.
- Branched service and volume hints survive into the prepared manifest.
- Source drift remains guarded by the existing prepared deploy baseline/source
  evidence paths.

### U3. Add CLI and request builder support

Files:

- Modify: `crates/ployzd/src/cli.rs`
- Modify: `crates/ployzd/src/request_builder.rs`
- Modify: `crates/ployzd/src/main.rs`
- Test: `crates/ployzd/src/main.rs`
- Test: `crates/ployzd/src/request_builder.rs`

Approach:

- Add `branch prepare SOURCE TARGET` with the same flags as existing branch
  namespace actions.
- Add `branch apply-prepared PREPARED_DEPLOY_ID`.
- Map prepare to `DaemonRequest::BranchNamespace` with
  `BranchNamespaceMode::Prepare`.
- Map apply-prepared to the existing `DaemonRequest::DeployApplyPrepared`.

Test scenarios:

- CLI parses `branch prepare prod pr-39 --volume data=branch`.
- Request builder encodes prepare mode and resource overrides.
- CLI parses `branch apply-prepared <id>`.
- Request builder emits `DeployApplyPrepared` with the supplied id.

### U4. Update e2e branch clone scenario

Files:

- Modify:
  `crates/ployz-e2e/src/scenarios/volume_clone_branch_real_smoke.rs`

Approach:

- Rework the branch command portion to call `branch prepare` with
  `--volume data=branch`.
- Parse the returned prepared deploy id from the daemon JSON response.
- Apply via `branch apply-prepared <prepared_deploy_id>`.
- Keep direct evidence assertions for independently writable cloned volume
  behavior.

Test scenarios:

- Real ZFS branch clone scenario prepares a branch deploy and then applies the
  prepared id.
- The final branch volume remains independently writable after source mutation.

### U5. Verification and review

Files:

- Relevant tests above.

Approach:

- Run formatting and targeted tests before review.
- Use subagent review for correctness, API contract, reliability, and testing.
- Fold actionable review findings into the branch before opening the PR.

Verification commands:

- `cargo fmt --check`
- `cargo test -p ployz-api branch`
- `cargo test -p ployzd branch`
- `cargo test -p ployz-e2e volume_clone_branch_real_smoke`
- `just test-all`

## Risks

- Direct `branch apply` may still be mistaken for an approved-preview flow.
  Documentation and CLI help should describe prepare/apply-prepared as the safe
  approval path.
- Reusing `DeployApplyPrepared` means metrics may show the apply as deploy-owned
  rather than branch-owned. That is acceptable for now because execution is
  deploy-owned.
- If e2e JSON parsing relies on response text rather than typed payloads, keep
  the helper small and local to the scenario.
