---
title: Slice 026 Deploy p2panda Command Surface Plan
status: completed
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-010-deploy-commit-drain.md
  - MVP/slice-018c-p2panda-deploy-restart-recovery.md
  - MVP/slice-023-deploy-candidate-cleanup-abi.md
  - MVP/slice-025-p2panda-net-substitution-consolidation.md
  - docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md
  - docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md
  - docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md
external:
  - https://docs.rs/state-machines/latest/state_machines/
  - https://docs.rs/ironflow/latest/ironflow/
  - https://docs.rs/tokio-util/latest/tokio_util/sync/
  - https://docs.rs/bon/latest/bon/
---

# Slice 026 Deploy p2panda Command Surface Plan

## Problem Frame

Deploy already proves the core product invariant: route/serving cutover is a
durable fact and drain is a consequence of that fact. The remaining leverage
problem is code shape. `mvp-deploy` owns useful domain types and a coordinator,
but the p2panda-backed deploy proof still carries deploy-specific fact writer
adapters in `MVP/e2e/src/deploy_restart_recovery_contract.rs`.

That is the same pattern Slice 024 fixed for ACME: product behavior is proven,
but reusable command-facing business plumbing still lives in an E2E canary.
Future deploy-like commands should not have to re-learn how to write deploy
decision, serving commit, and cleanup-done facts into the preferred p2panda
fact substrate.

This slice should extract the p2panda deploy command surface into a dedicated
`mvp-deploy-p2panda` adapter crate without changing the deploy semantics. The
E2E should keep participant fakes, serving/projection proof, metrics, and
crash/restart choreography. It should stop owning the deploy p2panda
fact-writing glue.

## Single Proof Target

`mvp-deploy-p2panda` exposes reusable p2panda-backed deploy and serving fact
writers. `deploy-restart-recovery-contract` uses those writers and deletes its
local deploy writer, serving writer, and p2panda outcome mapping. The E2E
keeps operation export/import orchestration because that is substrate proof
plumbing, not deploy command surface. The behavior preserved is:

- deploy decision fact before participant mutation,
- serving commit fact as the drain gate,
- cleanup-done fact for idempotent recovery,
- restart recovery from imported p2panda operations,
- no capacity/prepare/start replay after restart,
- projection catch-up before drain/stop,
- visible-node evidence in results.

## Requirements Trace

- `VISION.md`: commands need visible preconditions, bounded effects, clear
  results, and verification hooks. Deploy must fail loudly rather than
  presenting partial state as success.
- `MVP/overall-plan.md`: after Slice 025, the next proof should return to
  product semantic leverage and do the deploy command-surface equivalent of
  Slice 024.
- `MVP/architecture.md`: deploy state machines belong to deploy coordinator
  ownership; p2panda is the preferred durable fact substrate behind
  `FactSource`.
- `MVP/e2e-proof-plan.md`: E2E-6 and E2E-7 require commit-before-drain,
  restart recovery, projection-gated cleanup, and data-plane survival. E2E-9
  requires semantic-leverage evidence against the old deploy shape.
- `MVP/slice-018c-p2panda-deploy-restart-recovery.md`: p2panda deploy and
  serving writers were intentionally left E2E-local until the adapter shape
  survived more than one command. ACME and p2panda-net have now exercised the
  substrate enough to promote this deploy adapter.
- `MVP/slice-023-deploy-candidate-cleanup-abi.md`: candidate cleanup must stay
  explicit foreground participant RPC, not hidden background compensation.
- Institutional learning: preflight participants and durable intent before
  mutation; keep durable truth, projection, and live observation separate; and
  treat drain as deploy input rather than a reconciler.

## Dependency Scout

Checked before planning against current docs.rs pages on 2026-05-18:

- `state-machines` offers typestate and async state-machine macros. It is not a
  fit for this slice because deploy phase facts and recovery must remain
  explicit and persisted as Ployz facts, not hidden behind generated transition
  callbacks.
- `ironflow` is an event-sourced workflow engine with replay, outbox, and
  durable workflow concepts. It is too large for the current need and conflicts
  with the explicit non-goal of adding activity-replay or a workflow engine
  before the `PhasedCommand` trigger fires.
- `tokio-util` cancellation primitives remain useful later for actor/process
  shutdown, but this slice is about fact-writer extraction, not task
  cancellation.
- `bon` could reduce constructor/builder boilerplate, but adding a builder
  macro does not remove the deploy-specific substrate leak this slice targets.

Decision:

- Do not add a workflow/state-machine/builder dependency in this slice.
- Keep the existing explicit deploy state machine and traits.
- Add only a narrow p2panda adapter in `mvp-deploy-p2panda` so pure
  bus-backed deploy users and core deploy code do not pull in p2panda.

## Scope

In scope:

- Add p2panda-backed deploy fact writer and serving fact writer support in
  `MVP/deploy-p2panda` while keeping `MVP/deploy` core-only.
- Delete duplicated p2panda deploy writer code from
  `MVP/e2e/src/deploy_restart_recovery_contract.rs`.
- Keep trusted-author setup and operation import choreography in E2E. The
  shared store wrapper and `FactSource` implementation can move to
  `mvp-deploy-p2panda` because they are the deploy recovery read boundary.
- Keep p2panda details out of core deploy domain structs and out of the
  coordinator generic logic.
- Preserve the bus-backed deploy writer path for existing tests and canaries.
- Update semantic-leverage documentation with LOC/shape comparison.

Out of scope:

- Introducing `mvp-commands` or `PhasedCommand`.
- Rewriting `DeployCoordinator` into a generic workflow runner.
- Real Docker/ZFS/runtime deploy participant backends.
- p2panda-net deploy replication between real process roles.
- Quorum, witness acks, strict deploy leases, or consensus.
- Changing deploy semantics, phase ordering, projection catch-up, or cleanup
  policy.
- Moving non-MVP crates or changing the existing production deploy path.

## Design Decisions

### Keep p2panda At The Adapter Edge

`mvp-deploy-p2panda` can depend on both `mvp-deploy` and
`mvp-p2panda-facts`, but p2panda types must not leak into `DeployManifest`,
`DeployStateMachine`, participant wire payloads, or command results. The core
deploy API remains:

- `DeployFactWriter`,
- `ServingFactWriter`,
- `FactSource` for recovery/projection reads,
- `DeployCoordinator<W, S>` for orchestration.

The p2panda adapter should implement those traits. Feature code should see
deploy concepts, not p2panda operation internals.

### Promote The E2E Shape Only As Far As It Has Earned

The reusable surface should cover the deploy-specific part of what the
E2E-local adapter already proved:

- write deploy decision facts,
- write cleanup-done facts,
- write serving commit facts.

Do not add deploy-specific sync, network, process-role, or operation-copy
abstractions yet. The adapter may expose a shared fact-store wrapper because
deploy recovery and projection both need the same `FactSource` boundary, but
operation import/trust choreography stays in the E2E until a substrate slice
proves it belongs somewhere else.

### Preserve Projection Catch-Up As A Visible Gate

The extracted command surface must not collapse deploy into a one-shot
`execute_all` helper. Existing phases stay visible:

```text
execute_until_serving_commit -> projection catch-up -> finish_cleanup
```

The E2E must still assert that drain/stop happen after projection proof, not
merely after a method returns.

### No Workflow Engine Yet

This slice should deliberately not implement `PhasedCommand`. Deploy and ACME
now both have phase/resume-like shapes, but the design note says to lift the
primitive only when three or more command families repeat the pattern. This
slice should make deploy cleaner without hiding the state machine behind a new
framework.

## Implementation Units

### Unit 1: Characterize Current p2panda Deploy Adapter

Files:

- `MVP/e2e/src/deploy_restart_recovery_contract.rs`
- `MVP/deploy/src/facts.rs`
- `MVP/deploy/src/serving_commit.rs`
- `MVP/deploy/src/error.rs`
- `MVP/deploy/src/tests.rs`

Work:

- Record exactly which E2E-local types/functions are deploy business glue:
  `PandaDeployFactWriter`, `PandaServingFactWriter`,
  `coordinator_with_panda_facts`, and fact outcome mapping.
- Identify which pieces are test harness and should remain in E2E:
  trusted-author setup, operation import choreography, participant state,
  metric timings, process/serving choreography, and manifest fixtures.
- Do not add p2panda adapter tests in this unit. The feature and adapter do not
  exist yet, so p2panda-specific write-outcome tests belong to Unit 2.

Execution note: characterization-first. The first code commit should not change
E2E behavior; it should make the extraction target explicit through tests or a
small internal note if tests already cover it.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-restart-recovery-contract`

### Unit 2: Add p2panda Deploy Fact Adapter

Files:

- `MVP/Cargo.toml`
- `MVP/deploy-p2panda/Cargo.toml`
- `MVP/deploy-p2panda/src/lib.rs`

Work:

- Add a dedicated `mvp-deploy-p2panda` crate that depends on `mvp-deploy` and
  `mvp-p2panda-facts`.
- Keep `mvp-deploy` free of p2panda dependencies and feature gates.
- Implement `DeployFactWriter` and `ServingFactWriter` for p2panda-backed
  writers.
- The public adapter shape should be concrete and narrow:
  `PandaDeployFactStore`, `PandaDeployFactWriter`, and
  `PandaServingFactWriter`.
- `PandaDeployFactStore` wraps `PandaFactStore`, implements `FactSource`, and
  exposes operation export for restart proof choreography. It does not invent
  trusted author keys or authority.
- `mvp-deploy-p2panda` provides trait implementations and outcome mapping. The
  E2E provides trusted-author setup and import choreography for this slice.
- Preserve write outcome distinctions:
  - inserted,
  - already present,
  - deploy fact conflict with principal and content hash,
  - serving fact conflict with key.
- Keep p2panda author/session inputs explicit so transport identity never
  becomes authority.

Test scenarios:

- Decision write inserts and repeat write is already-present.
- Cleanup-done write inserts and repeat write is already-present.
- Serving commit write inserts and repeat write is already-present.
- Conflicting deploy decision returns `DeployFactConflict` with key, principal,
  and content hash.
- Conflicting serving commit returns `ServingFactConflict`.
- Core `mvp-deploy` construction does not require p2panda because the adapter
  is a separate crate.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy-p2panda`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy`
- `cargo check --manifest-path MVP/Cargo.toml -p mvp-deploy --no-default-features`

### Unit 3: Replace E2E-Local Deploy p2panda Glue

Files:

- `MVP/e2e/Cargo.toml`
- `MVP/e2e/src/deploy_restart_recovery_contract.rs`
- `MVP/e2e/src/deploy_commit_drain_contract.rs` if shared helper imports need
  adjustment

Work:

- Add the new `mvp-deploy-p2panda` crate to the MVP workspace and depend on it from `mvp-e2e`.
- Replace local p2panda deploy/serving writers with the `mvp-deploy-p2panda` adapter.
- Delete E2E-local fact outcome mapping.
- Keep E2E-local trusted-author setup and operation import choreography. The
  shared p2panda store wrapper and `FactSource` implementation move to
  `mvp-deploy-p2panda` so restart recovery and projection read from the same
  boundary used for writes.
- Keep E2E-local timing measurement as a wrapper/decorator around the reusable
  writer. Timing is scenario reporting, not part of the deploy adapter API.
- Keep participant simulation and serving/projection checks in E2E.
- Ensure the test still reads as product behavior:
  coordinator writes decision and serving commit, coordinator is dropped,
  operations are imported into a fresh store, recovery resumes cleanup after
  projection catch-up.

Test scenarios:

- Existing `deploy-restart-recovery-contract` metrics and assertions remain.
- No capacity/prepare/start requests are replayed after restart.
- Cleanup-pending and cleanup-done recovery stay idempotent.
- E2E file no longer defines deploy p2panda fact writers or fact outcome
  mapping.

Verification:

- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-restart-recovery-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-commit-drain-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-candidate-cleanup-contract`

### Unit 4: Documentation And Semantic-Leverage Ledger

Files:

- `MVP/slice-026-deploy-p2panda-command-surface.md`
- `MVP/e2e-proof-plan.md`
- `MVP/overall-plan.md`
- `MVP/primitive-decisions.md`

Work:

- Record what moved from E2E to `mvp-deploy-p2panda`.
- Compare LOC/shape before and after:
  - E2E-local p2panda deploy glue removed,
  - reusable deploy adapter added,
  - remaining E2E participant/projection/serving harness code.
- State explicitly that `PhasedCommand` remains deferred.
- Record the dependency scout decision not to add workflow/state-machine crates.
- Record the remaining deploy leverage target after this slice: reducing the
  large coordinator shape without hiding projection/drain gates.

Verification:

- Docs reference only repo-relative paths.
- `git diff --check`

## Review Risks

- The adapter crate could blur the domain/substrate boundary by growing beyond
  deploy fact writer and `FactSource` adaptation.
- A shared async store wrapper could hide writer lock failures or accidentally
  make sync `FactSource` reads block. This slice should avoid introducing that
  wrapper in core `mvp-deploy`.
- Moving outcome mapping could lose conflict detail.
- A convenience helper could hide projection catch-up or drain sequencing.
- The slice could overreach into `PhasedCommand` or deploy coordinator
  redesign.

Review should include correctness, maintainability, project standards,
reliability/failure behavior, and authorization/fact-boundary checks.

## Verification Gate

Targeted:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy-p2panda
cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy
cargo check --manifest-path MVP/Cargo.toml -p mvp-deploy --no-default-features
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-restart-recovery-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-commit-drain-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-candidate-cleanup-contract
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-deploy -p mvp-deploy-p2panda -p mvp-e2e --all-targets -- -D warnings
```

Closeout:

```bash
MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
git diff --check
```

## Done Criteria

- `deploy-restart-recovery-contract` no longer defines p2panda deploy fact
  writers or p2panda deploy outcome mapping.
- p2panda deploy/serving fact writers live in `mvp-deploy-p2panda` behind a narrow
  adapter surface, and `mvp-deploy` remains core-only.
- Existing deploy commit/drain, restart recovery, and candidate cleanup proofs
  still pass.
- No p2panda types leak into core deploy domain structs or coordinator command
  results.
- Operation export/import stays out of `mvp-deploy`; the deploy adapter writes
  facts, it does not become a replication harness.
- Semantic-leverage docs show whether the E2E became more product-focused and
  what deploy cleanup remains.
