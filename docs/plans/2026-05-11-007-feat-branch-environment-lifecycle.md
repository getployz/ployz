---
title: "feat: Persist branch environment lifecycle"
type: feat
status: active
date: 2026-05-11
origin:
  - VISION.md
  - docs/ideation/2026-05-10-pr-workflow-primitives-ideation.md
  - docs/plans/2026-05-11-003-feat-branch-command-plan-compiler.md
  - docs/plans/2026-05-11-004-feat-branch-prepare-apply-prepared.md
  - docs/plans/2026-05-11-006-feat-volume-clone-branching-hardening.md
---

# feat: Persist branch environment lifecycle

## Summary

Make branch environments first-class durable records in core. `branch prepare`
should create or update a branch capsule with source/target namespaces, compiled
mode choices, prepared deploy evidence, preview baseline, and lifecycle state.
`branch apply-prepared` should transition that same capsule to active after the
prepared deploy commits, or failed when apply fails. The dashboard and Inngest
can then treat core as branch truth instead of copying dashboard environments
and services per PR.

## Problem Frame

Branch currently exists as a compiler over deploy. That is the right execution
boundary, but downstream automation still has to reconstruct branch identity
from prepared deploys, deploy previews, and target namespaces. In
`ployz-dashboard`, that pressure pushes toward manually managed environment and
service rows per branch, which duplicates core state and makes PR merge/cleanup
logic a dashboard-specific model.

The core needs one durable object that answers: what branch environment exists,
what source produced it, what deploy prepared/applied it, what source modes were
used, and what state is it in now?

## Requirements

- R1. Add a serializable branch environment record in core types.
- R2. Use target namespace as the stable branch environment identity for v1.
- R3. `branch prepare` writes or replaces a branch record after deploy prepare
  succeeds.
- R4. The record stores source namespace, target namespace, default service and
  volume modes, resource overrides, prepared deploy id, manifest hash, preview
  baseline, service source evidence, volume clone evidence, image evidence,
  created/updated timestamps, and lifecycle state.
- R5. `branch apply-prepared` transitions the matching branch record to active
  after the prepared deploy apply succeeds.
- R6. Failed `branch apply-prepared` records failure against the matching
  branch record when a prepared deploy can be associated with a branch.
- R7. Add `branch status TARGET_NAMESPACE` and `branch list` so operators and
  cloud automation can read branch truth directly.
- R8. Keep branch execution deploy-owned. Branch records annotate lifecycle;
  they do not introduce another execution engine.
- R9. Keep dashboard/cloud-specific fields out of core. GitHub PR ids, TTLs,
  and cloud row ids belong downstream until a core primitive requires them.

## Scope

In scope:

- Core model types for branch lifecycle records.
- Store trait, memory store, store driver, and NATS KV persistence.
- Daemon branch prepare/apply-prepared lifecycle updates.
- Daemon branch status/list API and CLI commands.
- Tests covering serde, memory store, NATS decode/key safety, daemon lifecycle,
  and CLI request building.

Out of scope:

- Branch destroy/cleanup.
- Branch promote.
- GitHub PR metadata, TTL automation, dashboard database migrations.
- Portal services, data masking hooks, or provider-native database branches.
- Changing deploy execution semantics.

## Key Technical Decisions

1. Target namespace is the v1 branch key.
   Branch operations already require a source and target namespace, and the
   target namespace is the operator-visible environment handle. A future branch
   id can be added when multiple branch histories for one target become useful.

2. Store branch lifecycle separately from prepared deploy records.
   Prepared deploys remain deploy approval records. Branch records are the
   branch environment read model and lifecycle surface. This keeps deploy
   generic and gives cloud a branch-shaped primitive.

3. Branch apply-prepared gets its own daemon request.
   The CLI currently aliases `branch apply-prepared` to deploy apply-prepared.
   That loses branch lifecycle context. A branch-specific request can delegate
   to deploy apply-prepared and then update branch state.

4. Failure is operator-visible branch state.
   Expected apply failures still return deploy failure payloads to the caller,
   but the branch record should also preserve a compact failure code/message for
   the next reader.

5. Record evidence from deploy preview, not from ad hoc manifest parsing.
   Preview already carries service source evidence, volume clone work, image
   availability, manifest hash, and baseline. Reusing it avoids parallel
   dashboard-style inference.

## Existing Patterns

- `crates/ployzd/src/daemon/handlers/deploy.rs` already handles branch
  prepare by compiling a branch manifest and delegating to deploy prepare.
- `crates/ployz-types/src/model.rs` contains `PreparedDeployRecord`,
  `DeployPreview`, and durable deploy state types.
- `crates/ployz-store-api/src/traits.rs`,
  `crates/ployz-store-api/src/memory.rs`, and
  `crates/ployz-store-api/src/driver.rs` define the store contract and memory
  implementation pattern.
- `crates/ployz-nats/src/store/deploys/mod.rs` stores deploy status,
  prepared deploys, and deploy phases in authority-local durable KV buckets.
- `crates/ployz-nats/src/buckets.rs` classifies each authority-local stored
  intent bucket and has tests asserting the asset manifest.
- `crates/ployzd/src/request_builder.rs` and `crates/ployzd/src/main.rs`
  already parse and build branch commands.

## Implementation Units

### U1. Add Branch Lifecycle Model

Files:

- Modify: `crates/ployz-types/src/model.rs`
- Modify: `crates/ployz-api/src/deploy.rs`
- Test: `crates/ployz-types/src/model.rs`
- Test: `crates/ployz-api/src/deploy.rs`

Approach:

- Add `BranchEnvironmentState` with `Prepared`, `Active`, and `Failed`.
- Add `BranchEnvironmentFailure` with code/message and optional deploy id.
- Add `BranchEnvironmentRecord` keyed by target namespace.
- Add small API request/payload types for branch status/list/apply-prepared.
- Reuse `BranchResourceMode` and `BranchResourceModeOverride` for the stored
  resource mode evidence.

Test scenarios:

- Branch environment record serializes with prepared state and preview baseline.
- Failed state serializes with structured failure.
- API branch status/list/apply-prepared request shapes round-trip.

### U2. Persist Branch Records in Store

Files:

- Modify: `crates/ployz-store-api/src/traits.rs`
- Modify: `crates/ployz-store-api/src/driver.rs`
- Modify: `crates/ployz-store-api/src/memory.rs`
- Modify: `crates/ployz-nats/src/store/deploys/mod.rs`
- Modify: `crates/ployz-nats/src/buckets.rs`
- Test: `crates/ployz-store-api/src/memory.rs`
- Test: `crates/ployz-nats/src/store/deploys/mod.rs`
- Test: `crates/ployz-nats/src/buckets.rs`

Approach:

- Extend `DeployStore` with upsert/get/list branch environment methods.
- Use one authority-local durable KV bucket keyed by target namespace.
- Decode records defensively and reject key/payload mismatches.
- Keep list ordered by target namespace for stable output.

Test scenarios:

- Memory store upsert/get/list round-trips a prepared branch record.
- Upsert replaces the record for the same target namespace.
- NATS decode rejects branch record key mismatch.
- Asset manifest includes the branch environment bucket as stored intent.

### U3. Update Branch Prepare and Apply-Prepared Lifecycle

Files:

- Modify: `crates/ployzd/src/daemon/handlers/deploy.rs`
- Modify: `crates/ployzd/src/daemon/handlers/mod.rs`
- Test: `crates/ployzd/src/daemon/handlers/deploy.rs`

Approach:

- After branch prepare succeeds, extract the prepared deploy payload and upsert
  a prepared branch environment record.
- Add a branch-specific apply-prepared request handler that loads the prepared
  deploy id, delegates to deploy apply-prepared, then marks the matching branch
  active on success.
- On apply failure, mark the matching branch failed when the prepared deploy id
  maps to a branch record.
- Preserve the original deploy response and payload for callers.

Test scenarios:

- Branch prepare writes a prepared branch record with source/target namespaces,
  modes, prepared id, baseline, service branch sources, and volume clones.
- Branch prepare update for the same target namespace replaces prepared id and
  moves state back to prepared.
- Branch apply-prepared marks the matching branch active after deploy success.
- Branch apply-prepared failure records failure without hiding deploy failure
  payload.

### U4. Add Branch Status/List CLI Surface

Files:

- Modify: `crates/ployz-api/src/request.rs`
- Modify: `crates/ployzd/src/cli.rs`
- Modify: `crates/ployzd/src/request_builder.rs`
- Modify: `crates/ployzd/src/main.rs`
- Modify: `crates/ployzd/src/cli_io.rs`
- Test: `crates/ployzd/src/main.rs`
- Test: `crates/ployzd/src/cli_io.rs`

Approach:

- Add daemon requests for `BranchEnvironmentStatus` and
  `BranchEnvironmentList`.
- Add `branch status TARGET_NAMESPACE` and `branch list`.
- Keep JSON output as the full record/payload.
- Add compact plain rendering with target, source, state, prepared id, applied
  id, and failure summary when present.

Test scenarios:

- CLI parses status/list.
- Request builder emits the correct daemon requests.
- Plain output renders prepared/active/failed branch records predictably.

### U5. Verification and Review

Files:

- Relevant touched files.

Approach:

- Run focused tests before broad tests.
- Use subagent review for correctness, API contract, data integrity, reliability,
  and testing. Fold all valid findings into the PR.
- Open a non-draft PR and watch CI.

Verification:

- `cargo fmt --check`
- `git diff --check`
- `cargo test -p ployz-types branch_environment`
- `cargo test -p ployz-api branch`
- `cargo test -p ployz-store-api branch_environment`
- `cargo test -p ployz-nats branch_environment`
- `cargo test -p ployzd branch`
- `just test-all`

## Risks

- A branch-specific lifecycle record can drift from deploy truth if state
  updates happen before deploy commit. Success transitions must happen only
  after deploy apply returns success.
- Reusing target namespace as identity means a new prepare for the same target
  intentionally replaces the old branch capsule. This is acceptable for v1 and
  should be explicit in tests.
- Storing too much preview data can bloat KV entries. This slice stores
  evidence already intended for operator surfaces and avoids raw duplicated
  manifests beyond the prepared deploy pointer.
