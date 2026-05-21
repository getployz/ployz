---
title: Slice 033 Environment Branch Promote Rollback Plan
status: implemented
created: 2026-05-18
origin:
  - VISION.md
  - MVP/architecture.md
  - MVP/overall-plan.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/phased-command.md
  - MVP/routing/src/lib.rs
  - MVP/routing-p2panda/src/lib.rs
  - MVP/volume/src/command.rs
  - MVP/e2e/src/volume_transfer_contract.rs
  - MVP/e2e/src/process_role_harness.rs
---

# Slice 033 Environment Branch Promote Rollback Plan

## Problem Frame

The MVP has proven a lot of substrate and several single-operation product
canaries: deploy commit-before-drain, machine remove, ACME, p2panda-net
serving, and volume transfer. The north-star primitive surface in `VISION.md`
still lacks a proof for environment branching, promotion, and rollback.

This slice should add the smallest product-shaped environment command surface
that proves the useful semantic model:

- branch captures a source environment head as durable facts,
- promotion switches serving traffic to a branch head as a durable serving
  commit,
- rollback switches serving traffic back to the previous promoted head,
- gateway/DNS projections and process serving continue to work from facts while
  the local coordinator is absent,
- conflicts fail loudly before mutation and surviving races reduce
  deterministically.

This is not a full hosted preview-environment system. It is the first
foundation proof that branch/promote/rollback can be written as compact
business logic over the existing primitives instead of becoming a new
controller or desired-state layer.

## Scope

Include:

1. A new `mvp-environment` crate for typed environment domain, facts, command
   logic, and unit tests.
2. A new `mvp-environment-p2panda` adapter crate only if the core command needs
   p2panda-backed writers/readers outside the E2E harness. Keep the default
   plan to add it, because deploy/machine/routing already proved adapter crates
   keep product crates p2panda-free.
3. An E2E scenario named `environment-branch-promote-rollback-contract`.
4. p2panda-backed fact persistence and replay through `SharedPandaFactStore`.
5. Serving projection proof through the existing routing writer and process
   serving harness.
6. Semantic-leverage accounting against the old codebase if there is a clear
   old branch/promote/rollback surface to count; otherwise record that no
   honest baseline was identified.

Exclude:

- No generic workflow engine or `mvp-commands` crate in this slice.
- No activity replay, hidden compensation registration, or planner/executor
  split.
- No ZFS implementation. Volume forking is represented as a typed participant
  ABI and fact evidence, the same way volume transfer currently models
  snapshot/receive without production ZFS.
- No secrets or dataset implementation. This slice proves routing plus
  volume-ref state lineage. If secrets/datasets become part of the branch
  primitive, they need typed refs and participant evidence rather than being
  implied by environment facts.
- No automatic background reconciliation. Every mutation is command-owned.
- No CLI surface yet unless the current MVP pattern has already added one for
  equivalent product canaries. The E2E command API is enough for this slice.

## Crate Scout

Checked on 2026-05-18:

- `restate-sdk 0.10.0` exists and could model durable workflows, but it brings
  activity replay/service runtime semantics that conflict with the explicit
  no-replay `PhasedCommand` design note.
- `ironsaga 0.2.0` offers command pipelines with LIFO rollback, but the MVP
  needs persisted fact phases, visible operator-facing conflicts, and no hidden
  registered compensation closures.
- `workflow-rs 0.18.0` is an application framework, not a narrow command
  primitive for this architecture.
- `kotoba-workflow`/BPM-style crates target generic workflow specs. They would
  add a worldview rather than shrink the branch/promote domain.

Decision: do not add a workflow crate. Use existing crates and primitives:
`mvp-bus` for participant requests, `mvp-routing` for serving commits,
`mvp-p2panda-facts` for facts, `mvp-projection`/`mvp-serving` for
gateway/DNS proof, and `mvp-volume` domain ideas for volume evidence. Revisit
`mvp-commands` only after this slice if the environment implementation becomes
the third real persisted phase/resume command.

## Domain Model

Add typed identities in `mvp-environment`:

- `EnvironmentId`
- `EnvironmentBranchId`
- `EnvironmentCommandId`
- `EnvironmentHeadId`
- `EnvironmentEpoch`
- `EnvironmentVolumeRef`
- `EnvironmentRouteRef`

Fact shapes should be mostly immutable:

- `/facts/environment/<env>/head/<epoch>`
- `/facts/environment/<env>/branch/<branch_id>`
- `/facts/environment/<env>/promote/<command_id>`
- `/facts/environment/<env>/rollback/<command_id>`

The reducer rule should be deterministic and loud:

- current head is highest epoch, then content hash as stable tie-breaker,
- superseded candidate status is visible in projection/status,
- command entry reads relevant head/branch facts before first mutation and
  returns structured conflicts naming the competing fact.

`EnvironmentHeadFact` should carry:

- environment id,
- epoch,
- source command id,
- routing-owned serving commit id,
- previous head reference for rollback when the head came from promote,
- volume refs for stateful services,
- source branch id when applicable.

It must not duplicate gateway route payloads, DNS records, or active backend
lists. Those belong to routing/serving facts. Environment heads reference
routing-owned serving commits and volume refs; promote/rollback must obtain or
validate concrete serving payloads through routing-facing inputs.

`EnvironmentBranchFact` should carry:

- source environment,
- source epoch/head id,
- branch id,
- branch environment id,
- route refs,
- forked volume refs,
- visible nodes at decision time.

For this slice, branch heads may be seeded with a concrete routing input in the
command/E2E fixture. The branch command proves state lineage and branch fact
creation; it does not invent placement or backend generation.

## Command Semantics

### Branch

`BranchEnvironmentCommand`:

1. Reads the current source environment head.
2. Fails before mutation if the source head is missing, superseded, unreadable,
   or already changed from an explicit expected epoch.
3. Requests participant volume forks for each stateful volume ref.
4. Validates exact fork evidence: source volume, branch volume, source owner,
   target owner, snapshot/fork id, command id.
5. Re-reads the source environment head and fails before durable branch facts if
   the expected epoch changed during participant work.
6. Writes an immutable branch fact and an initial branch-environment head fact.
7. Returns visible nodes at decision time.

Branch should not alter production serving traffic.

### Promote

`PromoteEnvironmentCommand`:

1. Reads current production head and branch head.
2. Fails before mutation if the branch head is missing/unreadable or the
   production expected epoch no longer matches.
3. Builds or receives a `ServingCommitPlan` through routing-owned inputs. The
   environment command validates that the serving plan references the branch
   head and uses the current production serving commit as old-head evidence; it
   does not derive gateway/DNS payloads from environment facts.
4. Re-reads current production and branch heads immediately before any serving
   write.
5. Writes a promote decision fact before serving cutover. The decision records
   current production head, branch head, target serving commit id, target
   volume refs, and visible nodes.
6. Writes the serving commit through `mvp-routing::ServingFactWriter`.
7. Returns a pending promote result. A separate finalize step accepts
   `ProjectionCatchUp`, writes the promoted production environment head fact,
   and reports success.

Promotion's central invariant:

route cutover is a durable serving fact; old production head is rollback
evidence, not an implicit desired state.

Crash rule:

- if the decision exists but serving does not, recovery may retry serving;
- if serving exists but the promoted head is missing, recovery finalizes only
  after projection catch-up and after revalidating the decision;
- if the promoted head exists, recovery returns complete without rewriting
  serving.

### Rollback

`RollbackEnvironmentCommand`:

1. Reads the current production head and its previous promoted head reference.
2. Fails before mutation if no rollback target exists or the current expected
   epoch changed.
3. Builds or receives a routing-owned `ServingCommitPlan` that restores the
   rollback target route/DNS and volume refs.
4. Re-reads current production head immediately before serving write.
5. Writes a rollback decision fact before serving cutover.
6. Writes the serving commit.
7. Returns pending rollback. A separate finalize step accepts
   `ProjectionCatchUp`, writes a new production head fact with rollback
   evidence, and reports success.

Rollback is not "undo every side effect." It is a new forward command that
switches serving to the previous known-good head.

## Implementation Units

### Unit 1: Environment Domain and Fact Reducers

Files:

- `MVP/Cargo.toml`
- `MVP/environment/Cargo.toml`
- `MVP/environment/src/domain.rs`
- `MVP/environment/src/facts.rs`
- `MVP/environment/src/error.rs`
- `MVP/environment/src/lib.rs`
- `MVP/environment/src/tests.rs`

Plan:

1. Add typed newtypes with validation matching existing domain crates.
2. Add immutable fact key builders and decoders.
3. Add current-head reducer with deterministic conflict/superseded behavior.
4. Add structured errors for missing head, unreadable candidate, stale expected
   epoch, malformed payload, fact conflict, rollback target missing, and
   decision/head mismatch.

Tests:

- Current head selects highest epoch and records superseded candidates.
- Same-epoch conflict resolves deterministically by content hash.
- Malformed/wrong-key payloads are structured errors.
- Stale expected epoch fails before any command mutation.
- Head facts preserve volume-ref lineage; promote and rollback heads expose the
  expected current/previous volume refs without embedding gateway/DNS payloads.

### Unit 2: Core Commands

Files:

- `MVP/environment/src/command.rs`
- `MVP/environment/src/wire.rs`
- `MVP/environment/src/tests.rs`

Plan:

1. Define `EnvironmentFactWriter` and participant traits narrow enough for
   branch/promote/rollback.
2. Implement branch command with fork participant calls and exact evidence
   validation plus post-fork source-head revalidation.
3. Implement promote and rollback as begin/finalize command pairs using
   `ServingFactWriter` and caller-supplied `ProjectionCatchUp`.
4. Keep command state explicit; do not introduce `mvp-commands` unless the
   implementation proves the design-note trigger is met.

Tests:

- Branch writes branch/head facts after fork evidence and never writes serving.
- Branch rejects forged fork evidence.
- Promote writes a decision before serving, returns pending after serving, and
  refuses final success without projection catch-up.
- Promote recovery finalizes when decision and serving exist but head is
  missing.
- Rollback writes a decision before serving, returns pending after serving, and
  finalizes with a new forward head using previous head volume refs.
- Active conflict at command entry returns a structured conflict before
  participant calls.
- Stale head after participant work fails before durable commit.

### Unit 3: p2panda Adapter

Files:

- `MVP/Cargo.toml`
- `MVP/environment-p2panda/Cargo.toml`
- `MVP/environment-p2panda/src/lib.rs`

Plan:

1. Add `PandaEnvironmentFactWriter` over `SharedPandaFactStore`.
2. Keep `mvp-environment` p2panda-free.
3. Map p2panda inserted/already-present/conflict outcomes into
   environment-specific write outcomes and errors.

Tests:

- Writer records branch/head/promote/rollback facts.
- Repeated write is `AlreadyPresent`.
- Conflicting same-key fact is `FactConflict` with the key preserved.
- Projection/read principals cannot write and cannot call any adapter-owned
  trusted-replica import path. The environment adapter should not expose raw
  `import_operation`.

### Unit 4: E2E Product Proof

Files:

- `MVP/e2e/src/environment_branch_promote_rollback_contract.rs`
- `MVP/e2e/src/main.rs`
- existing process serving harness files only if the scenario needs a small
  reusable helper.

Plan:

1. Seed production environment head and serving state from p2panda-backed
   facts.
2. Run branch from `prod` to `pr-123`, including one forked volume ref and a
   concrete branch-serving input supplied by the scenario fixture.
3. Verify branch does not change production serving.
4. Promote the branch, observe pending state after serving write, then finalize
   with projection catch-up.
5. Verify gateway/DNS snapshots switch to branch backends and the production
   head's volume refs now match the branch refs.
6. Drop the command adapter/coordinator and prove serving continues.
7. Roll back through a fresh command instance reading p2panda-backed facts,
   observe pending state after serving write, then finalize with projection
   catch-up.
8. Delete `projections.sqlite`, rebuild, and verify gateway/DNS and environment
   head volume refs reflect the rollback head.

Assertions:

- visible nodes are included in every command result,
- no serving change before promote,
- promote serving commit precedes drain/old-backend removal semantics if any
  drain is modeled,
- rollback is a new forward head, not deletion of branch/promote facts,
- rollback restores the previous head's volume refs as durable state lineage,
- stale expected epoch rejects before fork/promote/rollback mutation,
- p2panda replay/rebuild recovers current head locally. Cross-island leakage
  stays covered by existing p2panda tests unless this slice introduces a new
  environment-specific island rule.

## E2E Registration and Gates

Add `environment-branch-promote-rollback-contract` to the scenario list and
the `all` suite.

Required checks:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-environment --all-targets`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-environment-p2panda --all-targets`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- environment-branch-promote-rollback-contract`
- `cargo clippy --manifest-path MVP/Cargo.toml --workspace --all-targets -- -D warnings`
- `MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all`

## Documentation and Leverage

Update:

- `MVP/overall-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/e2e-proof-plan.md`
- `MVP/design-notes/semantic-leverage-loc.md`

Record:

- whether `mvp-commands` is still deferred or now triggered,
- branch/promote/rollback domain LOC versus adapter/test/harness LOC,
- old-codebase baseline if an honest branch/promote/rollback surface can be
  identified,
- whether the slice extracted any volume p2panda adapter after repeating the
  volume store boundary.

## Risks

- Branch/promote/rollback can easily drift into a desired-state environment
  controller. Keep it command-shaped and explicit.
- Modeling volume fork evidence can duplicate volume-transfer mechanics. If
  duplication is meaningful, extract the p2panda/fact-store boundary into a
  second volume adapter only after both command paths are visible.
- Promotion and rollback should reuse routing's serving writer. If environment
  starts constructing projection payloads directly, the ownership boundary is
  wrong.
- If this slice adds a third persisted phase/resume command, stop and plan the
  `mvp-commands` primitive before expanding more command surfaces.

## Success Criteria

- Branch, promote, and rollback are represented as typed commands with
  structured errors and visible-node evidence.
- Environment facts are immutable and p2panda-backed.
- Promotion and rollback update serving only through routing's serving writer.
- Gateway/DNS process roles serve the promoted/rolled-back state from
  projections and survive command adapter absence.
- Projection rebuild and p2panda replay recover the current environment head.
- The implementation does not introduce background reconciliation or a generic
  workflow engine.
