---
title: Slice 023 Deploy Candidate Cleanup ABI Plan
status: completed
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/phased-command.md
  - MVP/slice-010-deploy-commit-drain-plan.md
  - MVP/slice-018-deploy-restart-recovery-plan.md
  - MVP/slice-022-p2panda-net-current-api-substitution.md
---

# Slice 023 Deploy Candidate Cleanup ABI Plan

## Problem Frame

Deploy already proves the central post-commit invariant: route cutover is a
durable fact, and old-backend drain is a consequence of that fact. The remaining
deploy gap is before serving commit. If a participant prepares or starts a
candidate instance and a later participant fails before serving cutover, the
current MVP relies on participant idempotency and manual inspection rather than
an explicit cleanup ABI.

This slice defines and proves that ABI without porting old `deploy.rs` and
without introducing the future `PhasedCommand` primitive early. The command
model stays foreground and no-quorum: the operator's connected node makes the
decision, writes durable local facts, reports visible nodes at decision time,
and cleans candidates best-effort with structured failure if a participant is
unavailable.

## Scope

Build the smallest deploy product proof for pre-serving candidate cleanup:

- candidate participants expose one explicit idempotent cleanup RPC,
- coordinator tracks prepare-attempted, prepared, and started candidate targets
  during the foreground deploy attempt,
- reversible pre-commit failure cleans those candidates before returning,
- recovery from a durable deploy decision with no serving commit can run the
  same cleanup without rerunning prepare/start,
- old backends are never drained or stopped before serving projection catch-up,
- cleanup failures are returned as structured, operator-visible state.

## Non-Goals

- Do not implement `mvp-commands` or the `PhasedCommand` trait in this slice.
- Do not add Temporal/Cadence/DBOS-style activity replay.
- Do not add rollback for irreversible phases.
- Do not add quorum, witness acks, or strict leases.
- Do not migrate production serving to Pingora or DNS to hickory-server here.
- Do not replace the p2panda-net `test_utils` harness in this slice.

## Dependency Scout

Sources checked:

- `changeset_saga` docs: https://docs.rs/changeset-saga/latest/changeset_saga/
- `tower` docs: https://docs.rs/tower/latest/tower/
- `tokio-util` `TaskTracker`: https://docs.rs/tokio-util/latest/tokio_util/task/task_tracker/struct.TaskTracker.html
- `bon` docs: https://docs.rs/bon/latest/bon/
- `typed-builder` docs: https://docs.rs/crate/typed-builder/latest

Decision:

- Do not adopt a saga/workflow crate. `changeset_saga` is explicitly a saga
  rollback helper, but the MVP's future command primitive intentionally avoids
  hidden activity replay and automatic compensation registration. This slice
  needs a named participant ABI and visible facts, not a generic workflow
  runtime.
- Do not add `tower` for participant RPCs. Tower's `Service` abstraction is a
  good request/response shape, but `mvp-bus` is already the substrate contract
  being tested. Wrapping it in Tower here would add one more layer before we
  have a repeated need.
- Defer `tokio-util` shutdown helpers. `CancellationToken`/`TaskTracker` remain
  good candidates for the production p2panda-net/process lifecycle slice, but
  candidate cleanup is foreground RPC plus facts.
- Do not add builder macros. The deploy types are small enough that hand-written
  constructors keep invariants clearer than a new proc-macro dependency.

## Key Decisions

### Candidate State Lives On Participants Until Commit

`PrepareInstance` and `StartInstance` remain participant RPCs. Participants must
label local runtime state with deploy identity, phase, instance id, service,
revision, and candidate role. Candidates are not routable until a serving commit
fact projects into gateway/DNS snapshots.

### Cleanup Is A Participant ABI, Not A Hidden Reconciler

Add a typed cleanup request for candidate state associated with a deploy. The
coordinator invokes it in two cases:

- during the same foreground deploy attempt when a reversible pre-commit phase
  fails after one or more candidates were prepared or started,
- during explicit recovery when a deploy decision fact exists but no matching
  serving commit fact exists.

No background loop should silently clean candidates. The caller receives the
result or a structured pending/failure value.

### Pre-Commit Cleanup Failure Is A Command Result

Post-serving cleanup already uses `CleanupStatus` and the
`/facts/deploy/<deploy_id>/cleanup/done` fact for old-backend drain/stop after a
serving commit. Candidate cleanup happens before serving commit and must use a
separate result surface. This slice should add an explicit pre-commit cleanup
result/pending type rather than forcing candidate cleanup into
`CleanupStatus`, which requires a serving commit today.

If candidate cleanup cannot reach a participant, the foreground command returns
a structured result naming the original deploy failure, visible nodes at
decision time, attempted candidate cleanup targets, and per-node cleanup
failure. It must not return success, and it must not pretend route cutover
occurred.

### Recovery Uses The Decision Fact And Participant Inspection

The deploy decision fact already contains the manifest and visible nodes. If
recovery finds that decision but no serving commit, it should not rerun
capacity, prepare, or start. It should send candidate cleanup to the planned
nodes from the manifest and record/report what happened.

### PhasedCommand Stays Deferred

This slice may make the phase pattern clearer, but it is still one command
family. Keep the logic explicit in `mvp-deploy`. After this slice, recount
deploy, ACME, membership, machine remove, and future volume work. If three or
more commands have the same phase/resume/compensate shape, plan
`mvp-commands` separately.

## Participant ABI

The slice should document and test this contract:

- `prepare_instance` is idempotent by `(deploy_id, instance_id)` and may leave
  local prepared candidate state.
- `start_instance` is idempotent by `(deploy_id, instance_id)` and may leave a
  locally running candidate.
- prepared or started candidates must not receive traffic until a serving commit
  is projected.
- `cleanup_deploy_candidates` is idempotent by `deploy_id` and removes only
  candidate state for the requested deploy.
- cleanup must handle both prepare-without-start and start-without-commit.
- cleanup must be safe after a prepare RPC timeout or handler failure, because
  the participant may have mutated local state before the coordinator observed
  the failure.
- cleanup must not remove active or draining instances from prior commits.
- failure to reach a participant is a foreground command result, not a log-only
  warning.

## Implementation Units

### Unit 1: Domain And Wire Types

Files:

- `MVP/deploy/src/domain.rs`
- `MVP/deploy/src/error.rs`
- `MVP/deploy/src/wire.rs`
- `MVP/deploy/src/lib.rs`
- `MVP/deploy/src/tests.rs`

Approach:

- Add typed domain values for candidate cleanup results and pending reasons.
  Reuse `NodeId`, `InstanceId`, `DeployId`, `VisibleNodes`, and
  `CleanupFailureKind`; do not introduce parallel visible-node or node-id
  types.
- Add a participant wire request for deploy candidate cleanup. Keep it
  node-scoped so one request can remove all candidate state for a deploy on that
  node.
- Add decoding/encoding tests for the new request and any reply shape.
- Keep request fields typed and variant-specific. No raw `Vec<String>` target
  lists.

Test Scenarios:

- cleanup request round-trips through `encode`/`decode`,
- cleanup request carries deploy id and planned instance ids as typed fields,
- pre-commit cleanup result distinguishes cleaned, pending, and not-needed
  cases without reusing post-serving `CleanupStatus`,
- cleanup failure classifications remain exhaustive over `DeployError`.

### Unit 2: Coordinator Pre-Commit Cleanup

Files:

- `MVP/deploy/src/coordinator.rs`
- `MVP/deploy/src/state_machine.rs`
- `MVP/deploy/src/tests.rs`

Approach:

- Track candidate targets before awaiting `prepare_instance`, after successful
  `prepare_instance`, and after successful `start_instance`. A prepare timeout
  or handler failure after request dispatch still requires candidate cleanup.
- On reversible pre-commit failure, send candidate cleanup to nodes that may
  hold candidates, then return a structured deploy result that names the
  original failure and cleanup success/pending status.
- If an irreversible phase has already committed, preserve today's loud
  `BlockedAfterIrreversiblePhase` behavior and do not pretend rollback exists.
- Add a recovery method for `DeployRecovery::PreCommitIncomplete` that cleans
  candidates from the manifest without rerunning capacity, prepare, start, or
  serving commit.

Test Scenarios:

- capacity failure still writes no decision and sends no cleanup,
- prepare failure after request dispatch writes decision and sends candidate
  cleanup for the attempted instance,
- start failure after prepare sends candidate cleanup for the prepared instance,
- later phase failure after a started candidate sends cleanup for started and
  prepared candidates,
- irreversible commit followed by later failure does not clean candidates as a
  fake rollback,
- recovery with decision/no serving commit cleans candidates and does not rerun
  prepare/start.

### Unit 3: Candidate Cleanup Result Fact Decision

Files:

- `MVP/deploy/src/facts.rs`
- `MVP/deploy/src/coordinator.rs`
- `MVP/deploy/src/tests.rs`

Approach:

- Default to no new durable fact. Prove command-returned structured cleanup
  results and explicit recovery from the existing decision/no-serving-commit
  facts first.
- If implementation proves a durable fact is required for operator status, use a
  separate candidate namespace such as
  `/facts/deploy/<deploy_id>/candidate_cleanup/<attempt_id>`. Do not reuse
  `/facts/deploy/<deploy_id>/cleanup/done`, which is reserved for post-serving
  old-backend cleanup tied to a serving commit.
- Do not add projection reducer participation for candidate cleanup in this
  slice. If a fact is introduced, E2E should read it directly through deploy
  fact helpers.

Test Scenarios:

- no new fact is needed to recover and clean from decision/no-serving-commit
  state, or, if the implementation must add one, the fact key/payload round
  trips in the candidate cleanup namespace,
- existing post-serving `DeployCleanupDoneFact` behavior and key remain
  unchanged,
- no projection reducer begins treating candidate cleanup as desired state.

### Unit 4: E2E Candidate Cleanup Contract

Files:

- `MVP/e2e/src/deploy_candidate_cleanup_contract.rs`
- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/deploy_commit_drain_contract.rs` only if a small helper can be
  extracted without making the E2E harder to read

Approach:

- Add a new scenario named `deploy-candidate-cleanup-contract`.
- Reuse the bus harness and deploy coordinator patterns from
  `MVP/e2e/src/deploy_commit_drain_contract.rs`.
- Keep the scenario business-readable: the proof should talk about candidates,
  serving commit absence, cleanup calls, and old backend drain counts, not
  transport plumbing.

Test Scenarios:

- prepare-only candidate is cleaned after start failure,
- started candidate is cleaned after a later participant failure,
- no serving commit fact exists after the failed pre-commit deploy,
- old backend drain/stop request counts stay zero,
- explicit recovery from decision/no-serving-commit cleans candidates without
  rerunning prepare/start,
- cleanup participant unavailable returns structured pre-commit pending/failure
  with the node id and `CleanupFailureKind`,
- visible nodes at decision time are present in the command result/report.

Metrics:

- visible nodes at decision time,
- prepared candidates,
- started candidates,
- candidate cleanup requests,
- cleanup pending count,
- prepare/start rerun count during recovery,
- old-backend drain/stop counts,
- elapsed milliseconds.

### Unit 5: Maintainer Documentation And Semantic Leverage

Files:

- `MVP/primitive-decisions.md`
- `MVP/e2e-proof-plan.md`
- `MVP/overall-plan.md`
- `MVP/slice-023-deploy-candidate-cleanup-abi.md`

Approach:

- Record the participant ABI and the no-hidden-reconciler rule.
- Update E2E proof status under deploy.
- Add a slice closeout report comparing the new candidate cleanup surface to the
  old deploy coordination baseline by behavior, not total repo LOC.
- Add a short `PhasedCommand` trigger note only: this slice either keeps the
  primitive deferred or names the exact third repeated command pattern that
  makes it urgent. Do not broaden into a full workflow inventory.

## Verification

Targeted checks:

```bash
cargo test -p mvp-deploy
cargo run -p mvp-e2e -- deploy-candidate-cleanup-contract
cargo run -p mvp-e2e -- deploy-commit-drain-contract
cargo run -p mvp-e2e -- deploy-restart-recovery-contract
cargo clippy -p mvp-deploy -p mvp-e2e --all-targets -- -D warnings
MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

## Review And Simplification Gates

- Run a simplification pass after Unit 2 or Unit 3, before E2E expansion makes
  the surface harder to reshape.
- Run review agents after the E2E passes. Include correctness, reliability,
  maintainability, testing, and project-standards review. Add security review if
  new fact authorization or session permissions are introduced.
- Address actionable findings before closing the slice.

## Risks

- Overbuilding this into a generic workflow engine would violate the current
  `PhasedCommand` trigger. Keep the cleanup ABI concrete.
- Returning both `Result<Err>` and a cleanup status result for the same failure
  can confuse callers. Pick one structured audience per path and test it.
- If execution discovers that a candidate cleanup fact is unavoidable, it can
  accidentally become desired state if named poorly. It must describe a
  completed foreground attempt, not an instruction for a background process to
  keep retrying.
- E2E helper extraction can hide business behavior. Extract only if it removes
  obvious repeated bus setup without obscuring the scenario.
