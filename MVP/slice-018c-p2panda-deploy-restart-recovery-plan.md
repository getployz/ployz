---
title: Slice 018c p2panda Deploy Restart Recovery Plan
status: planned
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution.md
  - MVP/slice-018-deploy-restart-recovery-plan.md
  - MVP/slice-018b-p2panda-fact-substrate.md
  - docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md
  - docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md
  - docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md
  - docs/solutions/performance-issues/machine-add-timeout-tests-2026-05-10.md
---

# Slice 018c p2panda Deploy Restart Recovery Plan

## Problem Frame

The deploy domain now has the important restart-recovery pieces in focused
code:

- deploy decision facts written before participant mutation;
- serving commit facts as the cutover boundary;
- cleanup-done facts;
- `recover_pending_cleanup` from durable facts;
- drain/stop gated by `ProjectionCatchUp`;
- focused tests for missing decision, missing serving commit, cleanup-done, and
  cleanup-pending after recovery.

Slice 018b then moved the preferred fact-substrate direction to p2panda-backed
signed operations. The remaining deploy proof is not another deploy state
machine. It is the substrate and E2E proof:

```text
deploy decision fact + serving commit fact + cleanup-done fact live in one
p2panda-backed fact store; the coordinator can die after serving commit and a
fresh coordinator resumes cleanup from those facts without re-running
pre-commit participant work.
```

This slice should keep deploy business logic small. The new work is adapter and
harness work around the existing seams, plus one end-to-end restart scenario.

## Requirements Trace

- `VISION.md`: the daemon/coordinator is disposable; the data plane outlives the
  control plane.
- `MVP/architecture.md`: route cutover is a durable fact; drain is a
  consequence of that fact; the fact/projection/serving roles survive
  coordinator death.
- `MVP/overall-plan.md`: the next product proof is deploy restart recovery and
  commit-before-drain rebuilt on the p2panda fact boundary.
- `MVP/e2e-proof-plan.md`: E2E must prove restart after serving commit before
  drain, projection rebuild, and steady-state serving while the coordinator is
  down.
- `MVP/primitive-decisions.md`: no quorum, no witness acknowledgements, and no
  hidden active-partition checks.
- `MVP/design-notes/p2panda-substitution.md`: p2panda owns operation envelopes,
  hashing, ingestion, and local operation storage; Ployz owns grants, reducers,
  and business semantics.
- `MVP/slice-018b-p2panda-fact-substrate.md`: `PandaFactStore` is local,
  session-bound, and currently uses a derived in-memory projection index.
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`:
  durable truth, projections, health, and live observation stay separate.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`:
  persist restart inputs before mutation and prove compatibility before
  changing authority-bearing state.
- `docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md`:
  drain is explicit operator intent; local and remote mutation paths must not
  blur lock ownership or RPC waiting.
- `docs/solutions/performance-issues/machine-add-timeout-tests-2026-05-10.md`:
  use operation-scoped short test policies instead of weakening failure-window
  semantics to make E2E fast.

## Scope

In scope:

- Add p2panda-backed deploy and serving fact writer adapters.
- Add the smallest p2panda operation exchange/import surface needed for an E2E
  fact role and projection role to share signed operations without using the bus
  fact store.
- Add `deploy-restart-recovery-contract`.
- Prove the restarted coordinator reads deploy decision, serving commit, and
  cleanup-done through one p2panda-backed `FactSource`.
- Prove no capacity/prepare/start work is re-run after restart.
- Prove no drain/stop happens before projection catch-up.
- Prove serving state continues from last-good snapshot while the coordinator
  object is gone.
- Update E2E proof docs and primitive decisions after implementation.

Out of scope:

- Real process death for the p2panda fact store.
- p2panda-net, discovery, blobs, or encrypted spaces.
- Persistent p2panda storage.
- New deploy phase facts, cleanup-started facts, attempt logs, or a global event
  log.
- `mvp-commands` / `PhasedCommand`.
- Real Docker/ZFS runtime operations.
- Pingora or production DNS migration.
- ACME on p2panda. The draft ACME plan remains future work after this proof.

## Crate Scout

No new dependency is needed for this slice.

Checked/current candidates:

- `mvp-p2panda-facts` already depends on `p2panda-core`, `p2panda-store`, and
  `p2panda-stream`. The missing capability is an MVP-local export/import API
  around the operations it already ingests.
- `p2panda-net` remains deferred because it would introduce another transport
  layer and is not needed to prove local signed-operation exchange.
- `tokio::sync::Mutex` can make writer adapters easy, but holding async locks
  around fact writes would make the substrate shape worse. Prefer explicit
  export/import handoff between stores over a shared async-locked store unless
  implementation proves that shape is materially simpler.
- Existing deploy/routing writer traits are already the correct seam:
  `DeployFactWriter` and `ServingFactWriter`.

## Design Decisions

### Keep p2panda Generic

`mvp-p2panda-facts` should not learn deploy, serving, routing, or projection
payload semantics. It may expose generic signed fact operation exchange:

```text
export_operations() -> Vec<PandaFactOperation>
import_operation(session, operation) -> PandaFactImportOutcome
```

The exact API can differ, but it must keep these rules:

- imported operations are validated through p2panda before becoming candidates;
- principal/session authority is still checked before local writes;
- imported operation metadata comes from signed header extensions;
- duplicate imports are idempotent;
- same-key/different-content imports remain conflict candidates;
- payload absence or invalid body hash is not treated as a valid payload.

If import support is too broad for this slice, keep it intentionally local:
enough for deterministic E2E operation exchange, not a production sync protocol.

### Deploy Writer Adapters Start In The E2E

Start with E2E-local adapters that translate `PandaFactWriteOutcome` into the
existing writer outcomes:

```text
PandaDeployFactWriter -> DeployFactWriter
PandaServingFactWriter -> ServingFactWriter
```

They should encode payloads using existing helpers:

- `deploy_decision_fact_payload`
- `deploy_cleanup_done_fact_payload`
- `serving_commit_fact_payload`

They should not duplicate deploy or serving serialization rules.

Do not add a p2panda dependency to `mvp-deploy` unless the E2E-local adapter
proves the shape should survive outside the harness. The expected core change
is smaller: make the existing written-fact result types constructible by an
external adapter without exposing bus internals.

### The E2E Kills a Coordinator, Not the Fact Role

The first proof can drop the coordinator object while leaving fact/projection
and serving harness objects alive. That matches the architecture: the
coordinator is the mutation owner; fact-sync/projection/serving are separate
steady-state roles.

The report must make the boundary explicit:

```text
killed: deploy coordinator
survived: p2panda fact role, projection role, serving state
```

Do not claim p2panda persistent-storage crash recovery until a later slice
actually kills and recreates the fact store from disk.

### Recovery Uses Stored Intent, Not Observation

The recovery path reads deploy decision and serving commit facts. It must not
infer deploy truth from:

- process liveness;
- gateway/DNS snapshots;
- participant health probes;
- projection status alone;
- stale in-memory `PendingCleanup`.

Projection catch-up is still required before destructive cleanup, but it is
evidence that serving state has caught up to a durable commit. It is not the
source of deploy truth.

## Implementation Units

### Unit 1: p2panda Operation Exchange

Files:

- `MVP/p2panda-facts/src/lib.rs`
- `MVP/e2e/src/p2panda_fact_source_contract.rs`

Work:

- Add an exported operation representation carrying signed p2panda header/body
  bytes or a similarly narrow typed operation value.
- Add export/import helpers on `PandaFactStore`.
- Preserve session-bound local writes.
- Preserve candidate statuses and payload reads after import.
- Keep operation import validation inside `mvp-p2panda-facts`.

Test scenarios:

- Export from one store and import into an empty store yields the same verified
  candidate and payload.
- Re-importing the same operation is idempotent.
- Importing a same-key/different-payload operation yields conflict candidates.
- Importing an operation for a reader without fact-read permission yields
  unauthorized status and no payload.

Verification:

- `cargo test -p mvp-p2panda-facts --lib`
- `cargo run -p mvp-e2e -- p2panda-fact-source-contract`

### Unit 2: Writer Result Unblock And E2E p2panda Writers

Files:

- `MVP/deploy/src/facts.rs`
- `MVP/deploy/src/serving_commit.rs`
- `MVP/deploy/src/tests.rs`
- `MVP/e2e/src/deploy_restart_recovery_contract.rs`

Work:

- Make `WrittenDeployFact` and `WrittenServingFact` constructible by external
  adapters, either through narrow public constructors or a small public
  `from_parts` API.
- Add E2E-local `PandaDeployFactWriter` and `PandaServingFactWriter` in the
  restart contract.
- Translate p2panda inserted/already-present/conflict into existing
  deploy/serving writer statuses and structured errors inside the E2E adapters.
- Keep the existing bus-backed writers for focused tests and existing E2E.
- Do not add p2panda dependencies to `mvp-routing`; serving fact payload helpers
  stay pure there.
- Do not add p2panda dependencies to `mvp-deploy` in this slice unless the
  E2E-local adapter becomes clearly duplicated by focused tests.

Test scenarios:

- external adapters can construct written deploy/serving fact results without
  accessing bus internals.
- p2panda deploy decision write returns inserted in the E2E adapter.
- duplicate decision write returns already-present in the E2E adapter.
- conflicting decision write returns `DeployFactConflict` with key, principal,
  and content hash in the E2E adapter.
- p2panda serving commit write returns inserted/already-present in the E2E
  adapter.
- conflicting serving commit write returns `ServingFactConflict`.
- writer authorization remains session-bound and cannot write as a forged
  principal.

Verification:

- `cargo test -p mvp-deploy --lib`
- `cargo clippy -p mvp-deploy --all-targets -- -D warnings`

### Unit 3: E2E Deploy Restart Recovery Contract

Files:

- `MVP/e2e/Cargo.toml`
- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/deploy_restart_recovery_contract.rs`
- `MVP/e2e/src/deploy_commit_drain_contract.rs` only for shared test helpers if
  extraction is cleaner than duplication.

Work:

- Add scenario `deploy-restart-recovery-contract`.
- Use the existing deploy participant fixture style: capacity, prepare, start,
  drain, and stop responders.
- Execute deploy until serving commit using p2panda-backed deploy and serving
  writers.
- Use one p2panda-backed write/read-view bridge for deploy decision, serving
  commit, projection source, recovery source, and cleanup-done. Avoid
  `DeployCoordinator::new`, because it would route facts through the bus.
- Project from the p2panda-backed `FactSource`, produce serving snapshots, and
  derive `ProjectionCatchUp`.
- Drop the original coordinator before cleanup starts.
- Start a fresh coordinator using the same participant bus but no prior
  `PendingCleanup`.
- Recover pending cleanup from p2panda-backed facts.
- Finish cleanup only after projection catch-up.
- Write cleanup-done through p2panda and prove a second recovery returns
  complete without RPC.

Required assertions:

- decision fact exists before participant mutation;
- serving commit exists before coordinator death;
- no drain/stop before projection catch-up;
- old backend remains in last-good serving state during coordinator outage;
- recovery reads stored intent instead of inferring deploy state from snapshots
  or participant health;
- restarted coordinator does not re-run capacity/prepare/start;
- restarted coordinator drains/stops after projection catch-up;
- cleanup-done suppresses repeat cleanup on second recovery;
- recovery status includes visible nodes and serving commit id on pending
  cleanup failure;
- existing `deploy-commit-drain-contract` remains green.

Metrics:

- decision write duration;
- serving commit to coordinator drop duration;
- projection catch-up duration;
- outage probes served from last-good state;
- recovery read duration;
- resumed drain duration;
- resumed stop duration;
- cleanup-done write duration.

Verification:

- `cargo run -p mvp-e2e -- deploy-restart-recovery-contract`
- `cargo run -p mvp-e2e -- deploy-commit-drain-contract`
- `MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all`

### Unit 4: Documentation And Proof Ledger

Files:

- `MVP/e2e-proof-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/overall-plan.md`
- `MVP/slice-018c-p2panda-deploy-restart-recovery.md`

Work:

- Record what is now proven on p2panda versus still only proven on bus/docs
  fixtures.
- Mark `deploy-restart-recovery-contract` in E2E-7.
- Record the adapter decision: deploy/routing semantics remain Ployz-owned;
  p2panda is only the fact envelope/store.
- Record that fact-store process death and persistent p2panda sync remain
  future work.

## Sequencing

1. Operation export/import first, because it is the substrate gap.
2. p2panda deploy/serving writers second.
3. E2E restart recovery third.
4. Documentation/report last.

Run the simplify workflow after Unit 2 or the first green E2E, whichever comes
first. Run code review with subagents after the full verification gate passes.

## Verification Gate

Minimum gate before shipping:

```text
cargo fmt --all
cargo clippy -p mvp-p2panda-facts -p mvp-deploy --all-targets -- -D warnings
cargo test -p mvp-p2panda-facts --lib
cargo test -p mvp-deploy --lib
cargo run -p mvp-e2e -- p2panda-fact-source-contract
cargo run -p mvp-e2e -- deploy-restart-recovery-contract
cargo run -p mvp-e2e -- deploy-commit-drain-contract
MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
git diff --check
```

## Risks

- Sharing one mutable p2panda store behind a lock would make the proof easy but
  blur the fact-role/projection-role boundary. Prefer explicit operation
  exchange if it stays small.
- The p2panda write API is async and mutable while `FactSource` is synchronous.
  If a shared bridge is simpler than operation exchange for this slice, keep it
  harness-local and document that it is a surviving fact role, not persistent
  p2panda process recovery.
- Import APIs can accidentally become a production sync protocol. Keep the API
  narrow until a real p2panda sync slice exists.
- Deploy may start depending directly on p2panda details. Keep p2panda in
  writer adapters and `FactSource`, not in state-machine logic.
- The E2E can accidentally prove object lifetime rather than durable facts. The
  fresh coordinator must recover from `FactSource`, not from a saved
  `PendingCleanup`.
- Existing untracked ACME planning should not be implemented before this proof
  unless the operator explicitly changes priority.

## Semantic-Leverage Check

Before implementation:

```text
rg -n "DeployFactWriter|ServingFactWriter|recover_pending_cleanup|PendingCleanup|ProjectionCatchUp" MVP/deploy MVP/e2e
```

After implementation, inspect whether the new code still reads as:

```text
preflight -> decision fact -> participant mutation -> serving commit
coordinator dies
read facts -> projection proof -> drain -> stop -> cleanup done
```

The slice is successful if the p2panda work is an adapter layer and E2E proof,
not another deploy framework.
