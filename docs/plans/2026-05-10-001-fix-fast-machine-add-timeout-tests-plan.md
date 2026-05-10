---
title: fix: Fast machine add timeout tests
type: fix
status: completed
date: 2026-05-10
---

# fix: Fast machine add timeout tests

## Summary

Make `just test-all` stop spending production timeout budgets in unit tests while preserving the production behavior of machine-add readiness waits and startup recovery cleanup.

## Assumptions

*This plan was authored in LFG pipeline mode without synchronous user confirmation. The items below are agent inferences that fill gaps in the input and should be reviewed before implementation proceeds.*

- Production timeout constants should stay operator-realistic; only test control over wait duration should change.
- The startup recovery test is intended to verify interrupted operation marking, not real SSH cleanup behavior.

## Requirements

- R1. The slow `ployzd` readiness-failure test must complete in milliseconds rather than waiting the 30 second production readiness timeout.
- R2. The interrupted machine-add startup recovery test must not accidentally hit real SSH timeout behavior.
- R3. Production machine-add timeout and cleanup semantics must remain unchanged.
- R4. Targeted tests and `just test-all` should demonstrate the runtime improvement.

## Scope Boundaries

- Do not shorten production readiness, RPC, cleanup, or SSH connect timeouts.
- Do not skip rollback, cleanup, or failure assertions to make the suite faster.
- Do not refactor unrelated machine-add, NATS, WireGuard, or SSH code.

## Context & Research

### Relevant Code and Patterns

- `crates/ployzd/src/daemon/handlers/machine/join/remote.rs` centralizes remote readiness, NATS readiness, and machine-record wait loops.
- `crates/ployzd/src/daemon/handlers/machine/join/rollback.rs` owns best-effort remote cleanup timeout wrappers.
- `crates/ployzd/src/daemon/handlers/machine/operations.rs` owns startup recovery for interrupted machine operations.
- `crates/ployzd/src/daemon/handlers/machine/tests.rs` already has fake SSH guards and focused machine-add tests around rollback and readiness behavior.

### Institutional Learnings

- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md` reinforces explicit operation phases and rollback visibility. The fix should keep the recovery path honest rather than hiding failures.

## Key Technical Decisions

- Keep production constants unchanged and introduce test-only control over wait durations where a handler-level test needs to exercise timeout behavior.
- Keep interrupted add-operation recovery cleanup-eligible for `BootstrapPublished`, because the remote join RPC may have been sent before the stage advances to `Joined`; tests should use fake SSH rather than real SSH to exercise that cleanup path quickly.
- Use targeted regression tests around the two slow paths instead of broad timing-only assertions.

## Open Questions

### Deferred to Implementation

- Exact helper shape for test wait policy: choose the smallest local pattern that does not leak into public API or production configuration.

## Implementation Units

### U1. Shorten Readiness Timeout Under Test Control

**Goal:** Allow machine-add readiness timeout tests to use a tiny timeout without changing production defaults.

**Requirements:** R1, R3, R4

**Dependencies:** None

**Files:**
- Modify: `crates/ployzd/src/daemon/handlers/machine/join/remote.rs`
- Test: `crates/ployzd/src/daemon/handlers/machine/tests.rs`

**Approach:**
- Add a narrow internal wait policy for remote readiness loops, defaulting to the current production constants.
- In tests, set that policy only around `machine_add_requires_sync_connected_for_running_joiner`.
- Preserve the same failure message shape so callers still see a timeout audience and last readiness failure context.

**Execution note:** Start by rerunning the exact slow test to confirm the baseline, then make the test fast without weakening its assertions.

**Patterns to follow:**
- Existing `TestSshEnvGuard` / `TestSshProgramGuard` scoped-test override style in `crates/ployzd/src/daemon/ssh.rs` and `crates/ployzd/src/daemon/handlers/machine/tests.rs`.

**Test scenarios:**
- Error path: remote readiness repeatedly returns `ready=false` and `sync_connected=false`; machine add fails with `MACHINE_ADD_FAILED`, reports `failed_ready: 1`, and preserves the timeout readiness message.
- Integration: failed readiness still removes the bootstrap machine record and WireGuard peer after rollback.

**Verification:**
- Exact run of `machine_add_requires_sync_connected_for_running_joiner` completes in well under one second and assertions remain behavior-focused.

### U2. Avoid Real SSH During Bootstrap-Published Recovery Tests

**Goal:** Ensure interrupted startup recovery marks the operation interrupted and preserves cleanup semantics without invoking real SSH in tests.

**Requirements:** R2, R3, R4

**Dependencies:** None

**Files:**
- Modify: `crates/ployzd/src/daemon/handlers/machine/operations.rs`
- Test: `crates/ployzd/src/daemon/handlers/machine/tests.rs`

**Approach:**
- Keep `BootstrapPublished` cleanup-eligible because the remote join RPC may already have reached the target before the operation stage advances.
- Use fake SSH in recovery tests so best-effort remote cleanup is exercised quickly without depending on real network timeouts.
- Continue remote cleanup even when local active mesh state is unavailable, recording both the skipped local cleanup note and the remote cleanup attempt.

**Execution note:** Characterize the current recovery test timing before changing the stage boundary.

**Patterns to follow:**
- `rollback_machine_add_target` stage checks in `crates/ployzd/src/daemon/handlers/machine/join/rollback.rs`.

**Test scenarios:**
- Error path: interrupted add operation at `bootstrap-published` is marked `Interrupted`, reports daemon restart, and attempts fake-SSH remote cleanup.
- Edge case: stages after remote join remain eligible for best-effort remote cleanup or explicit cleanup skips when only an operation-scoped SSH identity was available.
- Edge case: `active=None` startup recovery still attempts remote cleanup when the operation record has the needed network and target data.

**Verification:**
- Exact run of `interrupted_machine_add_is_marked_interrupted_on_startup` completes in well under one second.

## System-Wide Impact

- **Interaction graph:** Limited to `ployzd` machine-add test hooks and startup recovery.
- **Error propagation:** Failure messages should remain operator-visible and structured through existing `DaemonResponse` and operation notes.
- **State lifecycle risks:** The change must not leave bootstrap membership records behind when recovery can safely remove them.
- **Unchanged invariants:** Production readiness and SSH cleanup budgets remain unchanged.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Test-only timeout policy leaks into production configuration | Keep the override behind `#[cfg(test)]` or module-private constructors only. |
| Recovery skips cleanup for a stage that already touched the remote | Mirror the existing rollback stage boundary and cover post-join eligibility with a focused test if needed. |

## Verification Plan

- Exact test timing for `machine_add_requires_sync_connected_for_running_joiner`.
- Exact test timing for `interrupted_machine_add_is_marked_interrupted_on_startup`.
- `cargo test -p ployzd --lib`.
- `just test-all`.
