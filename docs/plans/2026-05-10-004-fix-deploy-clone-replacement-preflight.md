---
title: Centralize Deploy Clone Replacement Preflight
date: 2026-05-10
status: active
origin: review-comment-cluster
---

# Centralize Deploy Clone Replacement Preflight

## Problem

Recent review comments around deploy volume cloning were mostly one failure
class, not separate defects: deploy execution could replace or clean up a clone
dataset before the coordinator had one explicit view of runtime instances that
might still be using it.

The recurring symptoms were:

- clone cleanup could remove a dataset after an attached candidate had started,
- clone retry replacement could destroy a preserved target before stale writers
  were drained,
- removed or renamed stale candidates could be missed when cleanup was scoped
  only to the retry manifest's attached services,
- clone cleanup/retry logic drifted across phase failure, deploy failure, and
  backend stale-target replacement paths,
- clone participant RPC lane selection had to account for self-RPC behavior
  while deploy apply holds the shared daemon guard.

The structural fix should make the safe ordering hard to bypass:

1. Identify clone targets for the current phase.
2. Stop uncommitted namespace instances that are not represented by committed
   release slots.
3. Only then allow clone RPCs that may replace an existing uncommitted target.

This is deliberately narrower than a general transaction framework. Durable
clone artifact records, generalized rollback, and move-transfer cancellation
are deferred until they are needed by a concrete operation.

## Scope

In scope:

- Centralize clone target replacement preflight in
  `crates/ployz-orchestrator/src/deploy/execute.rs`.
- Run uncommitted instance cleanup once per clone execution batch instead of
  once per cloned volume.
- Keep the committed release-slot exemption so existing committed service
  instances are not drained before a clone retry.
- Rename events so diagnostics describe uncommitted instance cleanup, not only
  writer cleanup.
- Add regression coverage in
  `crates/ployz-orchestrator/src/deploy/tests.rs` for multiple cloned volumes
  sharing one stale uncommitted candidate.

Out of scope:

- Moving all stale-target replacement authority out of
  `crates/ployz-runtime-backends/src/storage/zfs.rs`.
- Adding a durable clone artifact registry.
- Adding volume move transfer cancellation or writer restoration.
- Changing the already-open clone RPC lane follow-up in
  `crates/ployzd/src/daemon/handlers/mod.rs`.

## Existing Patterns

- `crates/ployz-orchestrator/src/deploy/execute.rs` already owns deploy-time
  mutation ordering: clone volumes, move volumes, start candidates, then commit.
- `ParticipantSet::inspect` gives deploy execution a snapshot of runtime
  instances before mutation.
- `cleanup_uncommitted_volume_clones_after_failure` already treats started
  clone volumes as unsafe to clean up.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`
  establishes the relevant pattern: prove participants and eligibility before
  mutation.
- `docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md`
  establishes that request routing and local-vs-remote mutation are part of
  operation safety, not incidental plumbing.

## Implementation Units

### Unit 1: Clone Replacement Preflight Batch

Files:

- `crates/ployz-orchestrator/src/deploy/execute.rs`

Change:

- Gather phase-included planned clone volume names before the per-volume clone
  loop.
- If at least one clone is included, run one
  `stop_uncommitted_namespace_instances_before_volume_clones` pass.
- Compute committed instance ids from all current committed release slots in
  the resolved plan.
- Drain/remove inspected namespace instances that are not committed, failed, or
  removed.
- Remember stopped instance ids across phase clone batches so the same inspected
  candidate is not drained/removed repeatedly if clone work is split across
  phases.
- Emit `stop_uncommitted_instance` events that name the clone volumes being
  prepared.

Rationale:

- The deploy coordinator is the only layer that can see both clone intent and
  runtime instance status. Backend clone code can validate metadata, but it
  cannot know whether a container is still using a dataset.
- Running the preflight once avoids duplicate drain/remove RPCs for multiple
  cloned volumes in the same phase.

Test scenarios:

- Existing clone retry tests still drain stale uncommitted candidates before the
  first clone RPC.
- Existing committed-instance test still proves committed release slots are not
  drained.
- A retry with clone work split across phases drains/removes the same inspected
  stale candidate once, then runs every clone RPC after cleanup.

### Unit 2: Multi-Clone Regression

Files:

- `crates/ployz-orchestrator/src/deploy/tests.rs`

Change:

- Extend the removed stale candidate retry scenario so the retry manifest clones
  two volumes in the same execution batch.
- Assert the stale uncommitted instance is drained and removed exactly once.
- Assert both clone RPCs still run after the cleanup.

Rationale:

- The earlier per-volume helper made this bug easy: the immutable participant
  inspection snapshot was reused for every clone and would resend cleanup RPCs.
  The test should lock in one cleanup pass per batch.

## Risks

- The preflight is namespace-wide for uncommitted instances, not volume-specific.
  This is intentionally conservative while clone target metadata does not expose
  runtime mount ownership. The committed release-slot exemption keeps the pass
  from disrupting durable service state.
- If future deploy phases allow unrelated uncommitted candidates in the same
  namespace, this guard may stop them before clone replacement. That is still
  safer than replacing a dataset under a candidate with unknown volume usage,
  and it should be revisited when uncommitted candidates become first-class
  durable records.

## Verification

- `cargo test -p ployz-orchestrator volume_clone`
- `cargo check -p ployz-orchestrator`
- `cargo fmt --check`
- `git diff --check -- crates/ployz-orchestrator/src/deploy/execute.rs crates/ployz-orchestrator/src/deploy/tests.rs docs/plans/2026-05-10-004-fix-deploy-clone-replacement-preflight.md`
