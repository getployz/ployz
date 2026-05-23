---
title: "feat: Execute planned ZFS volume moves during deploy apply"
type: feat
status: active
date: 2026-05-09
origin: docs/plans/2026-05-08-004-feat-service-branching-deploy-plan.md
---

# feat: Execute planned ZFS volume moves during deploy apply

## Summary

Execute the volume move plans introduced by deploy intent hints by running the
existing ZFS snapshot/send/receive transfer path during deploy apply. The first
execution slice supports existing single-scope managed volumes and blocks until
transfer verification succeeds before candidate startup or durable deploy
commit. The actual ownership change remains the normal `DeployCommit`
boundary: transfer first while the store still says the source owns the volume,
then commit the updated `VolumeRecord.machine_id` with the release.

This is the first real state movement slice. It intentionally does not design
warm live copy or workload writer cutover beyond the existing recreate-style
deploy behavior.

## Problem Frame

The previous slice can plan a volume move and preview its source, target, and
attached-service placement, but apply still rejects move execution. That keeps
the plan safe, but it does not yet make `migrate` or machine-drain workflows
possible. Ployz needs the deploy apply path to perform the concrete ZFS transfer
and fail visibly before commit if the transfer, target authorization, or
snapshot verification fails.

The transfer must happen before the durable volume owner changes. The receiver
authorization in the ZFS transfer listener currently validates that the claimed
source machine owns the stored `namespace/volume`; committing the owner early
would cause the target listener to reject the source.

## Requirements

- R1. Deploy apply executes each planned `VolumeChange::Move` as foreground
  work before starting replacement candidates.
- R2. Transfer execution must use the existing ZFS transfer pieces: source
  snapshot, send/receive, target GUID response, and final GUID verification.
- R3. The deploy must fail before startup and before `commit_deploy` if any
  transfer stage fails.
- R4. A failed move after `DeployState::Applying` is written must be visible via
  the existing failed deploy status path.
- R5. Successful transfer does not itself mutate durable volume ownership.
  Ownership changes only through the existing deploy commit.
- R6. The source dataset is retained after a successful commit; deletion or
  cleanup is outside this slice.
- R7. Existing preview and plan-stability checks still guard source owner,
  target machine eligibility, and participant reachability before transfer.
- R8. The daemon/API participant path must block until the ZFS transfer reaches
  a terminal succeeded or failed state; async background transfer success is not
  enough for deploy apply.

## Scope Boundaries

- No warm live copy or incremental cutover policy beyond optionally reusing an
  existing `from_snapshot` transfer parameter when supplied by a future caller.
- No multi-volume atomic workload migration command UX.
- No source dataset deletion after commit.
- No new background reconciler or retry loop.
- No portal or snapshot-clone branch semantics.
- No new durable movement evidence store unless it is required to complete the
  blocking transfer; the moved `VolumeRecord`, deploy preview summary, and
  existing ZFS transfer records are enough for this slice.

## Context

- `crates/ployz-orchestrator/src/deploy/execute.rs` currently rejects moves via
  `ensure_volume_moves_are_not_executed`.
- `crates/ployz-orchestrator/src/deploy/participant.rs` only supports
  namespace inspect, candidate start, drain, and remove. It needs a blocking
  volume move operation.
- `crates/ployzd/src/daemon/handlers/volume/zfs.rs` already has
  `handle_volume_zfs_send`, `run_coordinated_zfs_transfer_inner`,
  `snapshot_on_machine`, `start_send_on_machine`, and transfer status records.
- `crates/ployzd/src/daemon/handlers/volume/transfer_listener.rs` validates
  source overlay identity and stored volume ownership before receiving a stream.
- `crates/ployz-api/src/request.rs` and `crates/ployz-api/src/volume.rs` already
  expose async ZFS send and transfer status payloads.
- `build_committed_volumes` already builds a moved `VolumeRecord` from the
  planned target machine once move execution is allowed.

## Key Technical Decisions

- Add a deploy participant volume-move method rather than teaching runtime
  backends about ZFS transfer. The operation is orchestration/daemon plumbing,
  not container runtime behavior.
- Execute moves after final plan stability and hostname validation, after the
  applying deploy record is written, and before `run_phase_startup`. This keeps
  failures visible while preserving the commit boundary.
- Keep transfer authorization based on current stored ownership. The deploy
  commit flips ownership only after transfer verification succeeds.
- Make the daemon deploy participant implementation call the existing ZFS send
  API and poll transfer status until `succeeded` or `failed`.
- Keep source retention explicit. A successful move leaves the source dataset in
  place for rollback/operator follow-up.

## Implementation Units

### U1. Add Deploy Participant Volume Move Contract

**Goal:** Give deploy apply a blocking participant operation for planned volume
moves.

**Files:**
- Modify: `crates/ployz-orchestrator/src/deploy/participant.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**
- Add `MoveVolumeRequest` and `MoveVolumeResult` near `StartCandidateRequest`.
- Include namespace/deploy context through the method arguments, and include
  `volume`, `from_machine`, `to_machine`, and generated snapshot name in the
  request.
- Extend fake deploy participant test client with deterministic success/failure
  hooks and operation logging.

**Test Scenarios:**
- Happy path fake participant records a move request before any start request.
- Error path fake participant returns an operation error that apply surfaces.

**Verification:**
- The orchestrator can express a planned move without reaching into daemon ZFS
  internals.

### U2. Execute Planned Moves in Deploy Apply

**Goal:** Replace the current move-execution guard with real foreground
execution before candidate startup.

**Files:**
- Modify: `crates/ployz-orchestrator/src/deploy/execute.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**
- Collect planned volumes whose `volume_record_change` is `VolumeChange::Move`.
- Generate deterministic deploy-scoped snapshot names, such as
  `ployz-deploy-<deploy-id>-<volume>`, sanitized through existing validation
  rules or a narrow helper.
- Call the participant move method for each move after `write_deploy_status`
  and before `run_phase_startup`.
- Emit deploy events for move start/success.
- Return errors immediately; the existing failed-deploy path should mark the
  applying record failed.

**Test Scenarios:**
- Happy path: move executes, service starts on target, commit updates
  `VolumeRecord.machine_id`.
- Error path: move failure writes failed deploy status, does not start
  candidates, and does not commit.
- Ordering: all planned move events happen before any start event.
- Stability: target lifecycle drift still fails before applying status and
  before move execution.

**Verification:**
- A deploy cannot report committed volume ownership unless the move call
  succeeded first.

### U3. Wire Daemon Deploy Participant to Blocking ZFS Transfer

**Goal:** Make real daemon-backed deploy apply use ZFS transfer and wait for a
terminal result.

**Files:**
- Modify: `crates/ployzd/src/daemon/handlers/deploy.rs`
- Modify: `crates/ployzd/src/daemon/handlers/volume/zfs.rs`
- Modify: `crates/ployz-api/src/request.rs`
- Modify: `crates/ployz-api/src/response.rs`
- Test: `crates/ployzd/src/daemon/handlers/volume/zfs.rs`

**Approach:**
- Reuse `VolumeZfsSend` to start transfer when possible, but add a small helper
  in the daemon volume module that returns or waits on `TransferRecord` status
  so deploy apply does not treat background dispatch as success.
- Poll `VolumeZfsTransferGet`/local transfer store with bounded interval and
  timeout from deploy participant code.
- Map terminal failed/interrupted transfer states to structured remote deploy
  errors.
- Preserve existing public async ZFS send behavior for CLI users.

**Test Scenarios:**
- Happy path helper observes a succeeded transfer and returns snapshot GUID and
  bytes transferred.
- Error path helper observes failed/interrupted transfer and returns an error.
- Error path missing transfer payload or timeout propagates a clear deploy
  participant error.

**Verification:**
- Daemon-backed deploy apply waits for verified transfer completion before
  returning from the move participant operation.

### U4. Preserve Transfer Listener Authorization

**Goal:** Ensure deploy movement does not weaken source ownership or overlay
  authorization checks.

**Files:**
- Modify: `crates/ployzd/src/daemon/handlers/volume/transfer_listener.rs`
- Test: `crates/ployzd/src/daemon/handlers/volume/transfer_listener.rs`

**Approach:**
- Prefer no production code changes if the existing authorization remains
  compatible.
- Add or adjust tests that prove receiver authorization accepts the pre-commit
  source owner and rejects a source that no longer owns the stored volume.

**Test Scenarios:**
- Happy path: source machine matching stored `VolumeRecord.machine_id` is
  accepted.
- Error path: target listener rejects source when store ownership has already
  changed.

**Verification:**
- The deploy implementation relies on the same transfer safety checks as manual
  ZFS send.

## Review Risks

- Polling an async transfer from deploy apply can accidentally hide background
  failure if it returns after transfer dispatch instead of terminal success.
- Committing volume ownership before transfer would break existing target
  authorization and create ambiguous durable truth.
- Starting the attached service before transfer verification would mount a
  missing or stale dataset.
- Treating ZFS transfer JSON records as authoritative deploy truth would blur
  operation evidence and durable ownership.

## Verification Plan

- `cargo test -p ployz-orchestrator`
- `cargo test -p ployzd volume::zfs`
- `cargo test -p ployzd volume::transfer_listener`
- `cargo check --workspace`

