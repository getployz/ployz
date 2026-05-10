---
title: "feat: Durable prepared deploys"
type: feat
status: active
date: 2026-05-11
origin:
  - VISION.md
  - docs/plans/2026-05-10-008-feat-deploy-preview-baseline-envelope.md
---

# feat: Durable prepared deploys

## Summary

Make "prepare then apply" a first-class deploy primitive. A prepare request
resolves the manifest, records the exact preview evidence and baseline, and
returns a durable prepared deploy id. A later apply-prepared request loads that
record, recomputes the plan from the stored manifest, and applies with the
stored baseline guard before participant work.

This turns preview/apply from a client-side convention into a core operation
surface that cloud, CLI, and agents can safely drive.

## Problem Frame

The baseline envelope slice made guarded apply possible, but clients still have
to carry `manifest_json` and `expected_baseline` correctly. That is an avoidable
footgun for the workflows this project wants: PR branches, tiered rollouts,
manual approval gates, Inngest-style automation, and future promote commands.

The core should expose the durable handoff object directly. The operator can
prepare a deploy, inspect the exact evidence, approve it, and then apply that
same prepared deploy id. Apply still verifies that the stored baseline matches
current cluster inputs before remote calls or mutation.

## Requirements

- Add a durable prepared deploy record to the core model.
- Prepared records must store:
  - prepared deploy id
  - namespace
  - manifest hash
  - manifest JSON
  - preview
  - baseline
  - coordinator machine id
  - created timestamp
  - expiry timestamp
  - state: prepared, applied, expired, superseded
- Add API request/response surfaces for prepare and apply-prepared.
- Add store methods for writing, reading, and state-updating prepared deploy
  records.
- Add daemon handlers that:
  - prepare using the same resolver/prober behavior as preview
  - apply prepared deploys by loading the stored manifest and baseline
  - reject missing, expired, already-applied, superseded, or malformed prepared
    records before participant work
- Preserve direct deploy apply for existing command-shaped use cases.
- Keep prepared deploys as explicit operator intent, not a background
  reconciler. No periodic expiration worker is required in this slice.

## Assumptions

- Prepared records are keyed by a generated `DeployId`-shaped id for now. A
  later UX slice may introduce a separate public id wrapper if needed.
- Expiration is enforced at apply/read time. Records may remain stored after
  expiry so operators can inspect what happened.
- Applying a prepared deploy may reuse the prepared id as the deploy id. This
  gives a single durable identity from preparation through execution and avoids
  a second correlation id.
- Superseding older prepared deploys for the same namespace is useful, but can
  be implemented as an explicit store state transition rather than an implicit
  cleanup loop.

## Scope

In scope:

- `crates/ployz-types/src/model.rs` prepared deploy record and state model.
- `crates/ployz-api/src/deploy.rs` prepare/apply-prepared request/response
  payloads.
- `crates/ployz-api/src/request.rs` daemon request variants.
- `crates/ployz-store-api/src/traits.rs`, `driver.rs`, `memory.rs` durable
  prepared deploy store operations.
- `crates/ployz-nats/src/store/deploys/mod.rs` NATS persistence for prepared
  deploy records.
- `crates/ployz-orchestrator/src/deploy/*` prepare and apply-prepared core
  functions.
- `crates/ployzd/src/daemon/handlers/deploy.rs` daemon handlers and response
  mapping.
- Focused unit/orchestrator/daemon/store tests.

Out of scope:

- CLI UX for prepare/apply-prepared.
- Dashboard/cloud consumption.
- Background expiration jobs.
- Rich list/filter APIs for prepared deploys.
- Merge/promotion semantics beyond applying the stored deploy.

## Key Decisions

1. Prepared deploys are records of operator intent, not desired state.
   They do not cause deploys to happen later by themselves. They are inert until
   an explicit apply-prepared command references them.

2. Apply-prepared uses the stored manifest and stored baseline.
   The caller supplies only the prepared id. The core owns the concurrency
   guard and fails if the cluster no longer matches the preview.

3. Prepared deploy state is explicit and monotonic.
   `prepared -> applied`, `prepared -> expired`, and `prepared -> superseded`
   are allowed terminal transitions. Applying a terminal record fails before
   participant work.

4. Expiry is checked synchronously.
   No reconciler is introduced. A record may be marked expired as part of a
   failed apply/read path if the store supports the update cleanly.

5. Prepare and preview share plan construction.
   The durable record must contain the same preview evidence a caller would see
   from deploy preview.

## Existing Patterns To Follow

- `crates/ployz-orchestrator/src/deploy/lifecycle.rs` already has an
  executor-local `PreparedDeploy`; durable prepared records should not duplicate
  execution internals, but should reuse preview and baseline evidence.
- `crates/ployz-orchestrator/src/deploy/mod.rs` is the thin public deploy API
  over plan/execute internals.
- `crates/ployz-store-api/src/traits.rs` and `crates/ployz-store-api/src/memory.rs`
  model durable store operations with explicit trait methods and focused tests.
- `crates/ployz-nats/src/store/deploys/mod.rs` stores deploy status records in
  a dedicated KV bucket; prepared deploys should follow that direct record
  pattern, not the deploy commit event stream.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`
  supports the shape: validate authority/preconditions before mutating durable
  truth.

## Implementation Units

### U1. Add prepared deploy model and API surface

Files:

- `crates/ployz-types/src/model.rs`
- `crates/ployz-api/src/deploy.rs`
- `crates/ployz-api/src/request.rs`

Approach:

- Add `PreparedDeployState` and `PreparedDeployRecord`.
- Add prepare response payload containing the stored record or prepared id plus
  preview evidence.
- Add apply-prepared request with a prepared deploy id.
- Add daemon request variants `DeployPrepare` and `DeployApplyPrepared`.

Test scenarios:

- Prepared deploy record JSON round-trips with snake_case state variants.
- API prepare/apply-prepared payloads preserve ids and preview/baseline data.
- Existing deploy preview/apply request JSON remains structurally valid except
  for intentional greenfield additions.

### U2. Persist prepared deploy records in stores

Files:

- `crates/ployz-store-api/src/traits.rs`
- `crates/ployz-store-api/src/driver.rs`
- `crates/ployz-store-api/src/memory.rs`
- `crates/ployz-nats/src/store/deploys/mod.rs`

Approach:

- Add `write_prepared_deploy`, `get_prepared_deploy`, and
  `mark_prepared_deploy_state` methods.
- Store prepared records by id.
- For NATS, use a deploy prepared KV entry keyed by prepared id. If adding a
  new bucket is too large for this slice, use the existing deploy status bucket
  only if record kinds remain unambiguous; otherwise add the bucket explicitly.
- State updates must fail if the record is missing.

Test scenarios:

- Memory store writes and reads prepared deploys.
- State transition updates only the target prepared id.
- Missing prepared id returns `None` on get and a structured store error or
  operation error on state update.
- NATS key helpers and encode/decode reject key-payload mismatches where
  existing deploy store tests already cover that pattern.

### U3. Add orchestrator prepare/apply-prepared functions

Files:

- `crates/ployz-orchestrator/src/deploy/mod.rs`
- `crates/ployz-orchestrator/src/deploy/execute.rs`
- `crates/ployz-orchestrator/src/deploy/plan.rs`
- `crates/ployz-orchestrator/src/deploy/tests.rs`

Approach:

- Add `prepare` that resolves the plan, validates hostname ownership,
  probes participants for preview warnings, builds a `PreparedDeployRecord`,
  writes it, and returns it.
- Add `apply_prepared` that loads the record, validates state/expiry/canonical
  baseline, decodes the stored manifest, and calls existing apply with
  `expected_baseline` from the record.
- Use the prepared id as the deploy id when applying.
- Mark the prepared record `applied` only after successful deploy commit. Keep
  failed deploy attempts visible through the normal deploy record failure path.

Test scenarios:

- Prepare stores preview evidence and baseline.
- Apply-prepared with matching state commits and marks the prepared record
  applied.
- Apply-prepared rejects missing ids, expired records, already-applied records,
  and malformed baselines before participant inspect/start.
- Apply-prepared rejects participant/source/volume drift via
  `DeployBaselineChanged` before participant work.

### U4. Add daemon handlers

Files:

- `crates/ployzd/src/daemon/handlers/deploy.rs`
- daemon request dispatch file if separate from the handler module

Approach:

- Add `handle_deploy_prepare` that mirrors preview setup and writes the durable
  prepared record.
- Add `handle_deploy_apply_prepared` that acquires the deploy lock for the
  prepared record namespace and delegates to orchestrator apply-prepared.
- Return structured errors for invalid options, missing prepared ids, expired
  prepared ids, and baseline drift.

Test scenarios:

- Prepare fails cleanly without active mesh.
- Prepare returns a record with preview and baseline.
- Apply-prepared rejects invalid/missing/expired records before mesh RPCs.
- Apply-prepared uses the stored manifest rather than caller-supplied manifest
  text.

### U5. Verification and PR hardening

Files:

- relevant tests above

Approach:

- Run focused tests for `ployz-types`, `ployz-api`, `ployz-store-api`,
  `ployz-orchestrator`, `ployz-nats`, and `ployzd`.
- Run `cargo fmt --check`, `git diff --check`, and `just test`.
- Use subagent code review with correctness, API contract, data integrity,
  reliability, and testing perspectives; fold all actionable findings back in.

Test scenarios:

- All local focused tests pass.
- Existing deploy apply/preview tests remain green.
- PR CI passes after push.
