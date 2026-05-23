---
title: "feat: Branch command plan compiler"
type: feat
status: active
date: 2026-05-11
origin:
  - VISION.md
  - docs/ideation/2026-05-10-pr-workflow-primitives-ideation.md
  - docs/plans/2026-05-10-003-feat-deploy-volume-snapshot-clone-branching.md
  - docs/plans/2026-05-10-006-feat-service-source-primitives.md
---

# feat: Branch command plan compiler

## Summary

Add the first `ployzctl branch` primitive as a compiler front-end over deploy.
The command takes a source namespace, target namespace, and explicit
per-resource source modes, then renders the existing deploy manifest shape with
service branch hints and volume clone hints. Branch does not introduce a new
execution engine; preview/apply still run through deploy.

## Problem Frame

Deploy can already represent service branch lineage and same-machine ZFS volume
snapshot clone branching. Operators still need to hand-author deploy manifests
to use those primitives together. That makes branch a collection of internals
rather than a product primitive.

This slice turns the shipped deploy vocabulary into a single command-shaped
operation while keeping unsupported modes honest. Portal attach and service move
remain rejected because they require separate safety models.

## Requirements

- R1. Add a branch request with `source_namespace`, `target_namespace`, mode,
  default service mode, default volume mode, and per-resource overrides.
- R2. Supported resource modes are `fresh` and `branch`.
- R3. Service `branch` compiles to `ServiceIntent::Branch` from the source
  namespace service of the same name. Service `fresh` emits no source hint.
- R4. Volume `branch` compiles to `VolumeIntent::Clone` with v1 explicit
  `raw` data and `crash_consistent` consistency. Volume `fresh` emits no source
  hint.
- R5. Portal, service move, provider branch, shared read-only, seed, and omit
  are not exposed in this skeleton.
- R6. The daemon owns compilation so CLI, SDK, and future cloud consumers share
  one semantics surface.
- R7. `ployzctl branch render-manifest` returns the compiled manifest.
- R8. `ployzctl branch preview` returns the deploy preview for the compiled
  manifest.
- R9. `ployzctl branch apply` applies the compiled manifest through the existing
  deploy apply path.
- R10. Duplicate or unknown resource overrides fail before deploy mutation.

## Scope Boundaries

In scope:

- API request/response model for branch namespace compilation.
- Daemon handler that exports the source namespace and compiles a target deploy
  manifest.
- CLI parser/request builder for `branch render-manifest`, `branch preview`,
  and `branch apply`.
- Unit coverage for request serialization, CLI request building, compiler
  behavior, and rejection paths.

Out of scope:

- PR capsule records, GitHub integration, TTL cleanup, and promotion.
- Portal services or attaching a branch service to production resources.
- Service moves or migration inside branch.
- Cross-machine volume clone/copy.
- Data masking hooks, seed hooks, and secrets policy.

## Key Technical Decisions

1. Branch compiles to deploy manifests, not a new apply path.
   This preserves deploy as the single mutating primitive boundary and lets
   existing preview baseline, image availability, phase, clone, and commit
   behavior do the hard work.

2. Daemon-side compilation is the durable API surface.
   The CLI should not export and rewrite manifests locally because cloud and
   agents need the same operation as a typed daemon request.

3. Default services branch; default volumes are fresh.
   Service branch is safe lineage over service spec revisions. Volume branch
   copies production-derived data, so users must opt in with
   `--volume data=branch` or `--volume-mode branch`.

4. Per-resource overrides are validation inputs, not patch language.
   The compiler only changes source modes. It does not edit images, env vars,
   routes, secrets, or placement in this slice.

## Existing Patterns

- `crates/ployzd/src/daemon/handlers/deploy.rs` already renders migrate
  manifests and applies them through deploy.
- `crates/ployzd/src/request_builder.rs` already builds `migrate` requests with
  render/preview/apply modes.
- `crates/ployz-types/src/spec.rs` already contains `ServiceIntent::Branch` and
  `VolumeIntent::Clone`.
- `crates/ployz-orchestrator/src/deploy/plan.rs` already resolves service
  branch and volume clone hints into preview/apply work.

## Implementation Units

### U1. Add API Branch Request Shape

**Files:**

- Modify: `crates/ployz-api/src/deploy.rs`
- Modify: `crates/ployz-api/src/request.rs`
- Test: `crates/ployz-api/src/deploy.rs`
- Test: `crates/ployz-api/src/request.rs`

**Approach:**

- Add `BranchNamespaceMode`, `BranchResourceMode`,
  `BranchResourceModeOverride`, and `BranchNamespaceRequest`.
- Add `DaemonRequest::BranchNamespace { request }`.
- Keep enum values serialized as snake_case.

**Test Scenarios:**

- Branch request round-trips with branch/fresh modes and overrides.
- Daemon request round-trips.

### U2. Add Daemon Branch Compiler

**Files:**

- Modify: `crates/ployzd/src/daemon/handlers/deploy.rs`
- Modify: `crates/ployzd/src/daemon/handlers/mod.rs`
- Modify: `crates/ployzd/src/metrics.rs`
- Test: `crates/ployzd/src/daemon/handlers/deploy.rs`

**Approach:**

- Export the source namespace manifest with the existing store-backed exporter.
- Rewrite `manifest.namespace` to the target namespace.
- Resolve service and volume modes from defaults plus overrides.
- Add service branch and volume clone intent hints.
- Validate the compiled manifest.
- For render mode, return the manifest.
- For preview/apply modes, delegate to existing deploy preview/apply handlers.

**Test Scenarios:**

- Default compilation branches all services and leaves volumes fresh.
- Volume override compiles to `VolumeIntent::Clone`.
- Service override to fresh removes that service hint.
- Unknown or duplicate overrides return structured command errors before apply.

### U3. Add CLI Surface

**Files:**

- Modify: `crates/ployzd/src/cli.rs`
- Modify: `crates/ployzd/src/request_builder.rs`
- Modify: `crates/ployzd/src/main.rs`
- Test: `crates/ployzd/src/main.rs`
- Test: `crates/ployzd/src/request_builder.rs`

**Approach:**

- Add `branch render-manifest SOURCE TARGET`.
- Add `branch preview SOURCE TARGET`.
- Add `branch apply SOURCE TARGET`.
- Add `--service-mode fresh|branch`, `--volume-mode fresh|branch`,
  repeatable `--service name=fresh|branch`, and repeatable
  `--volume name=fresh|branch`.

**Test Scenarios:**

- CLI parses all three branch subcommands.
- Request builder maps flags to the branch request.
- Invalid resource override syntax fails with usage error.

### U4. Add E2E/Integration Coverage

**Files:**

- Modify: `crates/ployz-e2e/src/scenarios/volume_clone_branch_real_smoke.rs`
  or add a focused branch compiler scenario if runtime cost is acceptable.
- Modify: `crates/ployz-e2e/src/scenarios/mod.rs` if adding a scenario.

**Approach:**

- Prefer extending the existing real ZFS clone branch scenario to drive the
  branch command rather than a hand-written clone deploy manifest.
- Keep the scenario real-ZFS only.

**Test Scenarios:**

- `branch preview prod pr-39 --volume data=branch` reports service branch
  lineage and volume clone work.
- `branch apply prod pr-39 --volume data=branch` produces an independently
  writable cloned volume.

## Verification

- `cargo fmt --check`
- `cargo test -p ployz-api`
- `cargo test -p ployzd branch --lib`
- `cargo test -p ployz-e2e`
- `just test-all`
- PR CI

## Risks

- Defaulting volumes to branch would silently copy production data. Keep fresh
  as the default until a data safety ladder exists.
- Rendering branch as a manifest may tempt downstream systems to patch raw
  manifest JSON. That is acceptable for now, but future cloud should use typed
  overrides rather than ad hoc rewrites.
- Branch apply inherits deploy's current semantics, including image
  availability requirements. The branch command should surface those failures
  unchanged instead of masking them.
