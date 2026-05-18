---
title: Slice 027 Routing-Owned Serving Commit Plan
status: completed
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-026-deploy-p2panda-command-surface.md
  - docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md
external:
  - https://docs.rs/async-trait/latest/async_trait/
  - https://docs.rs/trait-variant/latest/trait_variant/
  - https://docs.rs/enum_dispatch/latest/enum_dispatch/
reviewed_by:
  - ce-feasibility-reviewer
  - ce-scope-guardian-reviewer
---

# Slice 027 Routing-Owned Serving Commit Plan

## Problem Frame

Slice 026 made deploy p2panda fact writing reusable without letting p2panda leak
into core deploy. The next cross-command leak is now visible: the serving
commit writer trait lives in `mvp-deploy`, but serving cutover is not a deploy
concept. Machine remove also commits serving state, and today it bypasses a
writer abstraction by calling `mvp_routing::write_serving_commit` directly.

That shape will force every future command that changes routes to choose
between importing deploy internals, duplicating serving write handling, or
writing directly to the bus. The primitive should instead be:

```text
routing owns serving commit facts
commands depend on a serving fact writer
transport-specific serving writers live at adapter edges
```

This slice should correct ownership and inject that primitive into deploy and
machine remove. It should not convert machine remove to p2panda yet; reviewer
feedback showed that p2panda machine remove needs its own explicit decisions
around join-fact inputs and machine error mapping.

## Single Proof Target

`mvp-routing` owns the serving fact writer contract. `mvp-deploy` and
`mvp-machine` consume that contract instead of each owning or bypassing serving
write semantics. Existing deploy and machine-remove E2Es continue to prove:

- deploy decision before serving commit,
- serving commit as the drain/stop gate,
- projection catch-up before cleanup,
- machine remove stop only after projection catches up,
- tombstone only after stop,
- visible nodes at decision time are still returned.

## Requirements Trace

- `VISION.md`: operations are explicit commands, the daemon is disposable, and
  data-plane behavior must outlive control-plane mutation.
- `MVP/overall-plan.md`: the next slice should add product logic on top of
  existing primitives instead of growing generic substrate again.
- `MVP/e2e-proof-plan.md`: E2E-6 and E2E-7 require commit-before-drain and
  crash/steady-state behavior; E2E-9 requires measuring semantic leverage.
- `MVP/primitive-decisions.md`: p2panda is the preferred durable fact substrate,
  but feature slices should distinguish product leverage from new shared
  foundation LOC.
- `MVP/slice-026-deploy-p2panda-command-surface.md`: p2panda adapters belong at
  adapter edges, while core command crates stay substrate-free.
- `docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md`:
  drain/remove behavior must be target-aware, avoid deadlocking local mutation,
  and treat drain intent as deploy/remove input rather than background truth
  rewriting.

## Dependency Scout

Checked before planning on 2026-05-18:

- `async-trait` would let writer traits use `async fn`, but it expands to the
  same boxed-future shape the MVP already uses. Do not add the macro for this
  slice.
- `trait-variant` helps publish Send/non-Send variants of async traits, but it
  does not solve the object-safe writer boundary better than the current
  explicit `Pin<Box<dyn Future>>` pattern.
- `enum_dispatch` can remove dynamic-dispatch overhead for a closed set of
  implementors, but these writer traits are generic adapter seams, not hot-path
  polymorphic loops.

Decision: add no dependency. Keep the current explicit async trait style.

## Scope

In scope:

- Move serving fact writer types from `mvp-deploy` to `mvp-routing`:
  `WrittenServingFact`, `ServingFactWriteStatus`, `ServingFactWriter`, and
  `BusServingFactWriter`.
- Update deploy coordinator/tests to use the routing-owned writer contract.
- Move the p2panda serving writer out of `mvp-deploy-p2panda` and into a
  routing adapter edge.
- Update `mvp-deploy-p2panda` to compose that routing p2panda writer instead
  of owning a deploy-named serving writer.
- Update `MachineRemoveCoordinator` to accept a serving writer generic instead
  of writing directly to the bus.
- Keep `machine-remove-contract` on its existing iroh-docs removal facts and
  bus-backed serving facts for this slice, but make the serving write flow
  travel through the injected routing writer.
- Record a LOC/maintenance ledger for this ownership correction.

Out of scope:

- p2panda machine-remove facts.
- p2panda join facts or a mesh p2panda adapter.
- Editing `mvp-p2panda-facts` for a shared store handle.
- `PhasedCommand`.
- Production Pingora/DNS migration.
- Real runtime participant backends for machine remove.
- Machine add/join changes.
- Consensus, witness acks, quorum writes, or strict leases.
- p2panda-net cross-process machine remove replication.

## Design Decisions

### Serving Commit Is Routing State

Serving facts drive gateway and DNS projection. They should be owned by
`mvp-routing`, not `mvp-deploy`. Deploy is one producer of serving commits;
machine remove, promote, rollback, and future branch operations are others.

The routing contract should carry routing errors, not deploy errors. Deploy can
map those errors through `DeployError::from`, and machine remove can map them
through `MachineRemoveError::from`.

### The p2panda Adapter Should Be Narrower Than A Store Facade

Do not introduce a generic p2panda store handle in this slice. Reviewer
feedback showed that the repeated problem here is typed writer ownership, not
yet a proven need for another shared store abstraction.

`mvp-routing-p2panda` should own a narrow p2panda serving-writer adapter. If it
needs to be generic over storage, prefer a tiny write-sink trait scoped to
serving fact writes over a broad cloneable store facade. Do not edit
`mvp-p2panda-facts` unless implementation proves two current adapter crates
would otherwise duplicate the same store/`FactSource` wrapper.

### Machine Remove Should Not Import Deploy

Machine remove must not depend on `mvp-deploy` just to write serving commits.
If a command needs route cutover, it depends on routing. If it needs
machine-remove facts, it depends on machine. If it needs p2panda, that belongs
in a future `mvp-machine-p2panda` slice with explicit error mapping and
join-fact decisions.

### Keep Projection Catch-Up Visible

The machine remove sequence must stay explicit:

```text
write removal-started
prepare target no-new-work/drained
write serving commit through routing writer
project serving commit
stop removed workloads
write tombstone
```

Do not hide this behind a one-shot helper. The E2E must still assert the event
ordering.

## Implementation Units

### Unit 1: Move Serving Writer Contract To Routing

Files:

- `MVP/routing/src/lib.rs`
- `MVP/deploy/src/serving_commit.rs`
- `MVP/deploy/src/coordinator.rs`
- `MVP/deploy/src/lib.rs`
- `MVP/deploy/src/tests.rs`
- `MVP/e2e/src/deploy_restart_recovery_contract.rs`

Work:

- Move `WrittenServingFact`, `ServingFactWriteStatus`, `ServingFactWriter`, and
  `BusServingFactWriter` to `mvp-routing`.
- Make the writer trait return `RoutingResult<WrittenServingFact>`.
- Update `DeployCoordinator` to use `mvp_routing::ServingFactWriter` and map
  errors through `DeployError`.
- Update deploy tests and E2E timing wrappers to import serving writer types
  from routing.
- Avoid compatibility re-exports unless they prevent needless churn inside the
  same slice. This is MVP-internal code, so prefer one canonical owner.

Test scenarios:

- Deploy serving commit success still writes decision before serving fact.
- Conflicting serving commit remains a structured serving conflict.
- Recovery after serving commit still resumes cleanup after projection.
- `mvp-deploy` no longer defines serving writer ownership.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-routing`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-restart-recovery-contract`

### Unit 2: Rehome p2panda Serving Writer At The Routing Edge

Files:

- `MVP/Cargo.toml`
- `MVP/routing-p2panda/Cargo.toml`
- `MVP/routing-p2panda/src/lib.rs`
- `MVP/deploy-p2panda/Cargo.toml`
- `MVP/deploy-p2panda/src/lib.rs`
- `MVP/e2e/Cargo.toml`
- `MVP/e2e/src/deploy_restart_recovery_contract.rs`

Work:

- Add `mvp-routing-p2panda` with `PandaServingFactWriter`.
- Keep the adapter narrow. It should write serving commit key/payloads and map
  p2panda write outcomes into routing's serving write result.
- Do not create a broad shared p2panda store handle.
- Update `mvp-deploy-p2panda` so deploy-specific writers stay there, while
  serving writer ownership moves to `mvp-routing-p2panda`.
- If a generic sink is needed, define it in `mvp-routing-p2panda` and implement
  it for `PandaDeployFactStore` inside `mvp-deploy-p2panda`. This avoids
  making routing depend upward on deploy while keeping the existing deploy
  restart E2E store shape usable.
- Preserve inserted/already-present/conflict distinctions.
- Keep p2panda author/session inputs explicit.

Test scenarios:

- p2panda serving commit insert returns `Inserted`.
- Repeat serving commit returns `AlreadyPresent`.
- Conflicting serving commit returns `RoutingError::ServingFactConflict`.
- Deploy p2panda writer tests still pass after serving writer moves out.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-routing-p2panda`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy-p2panda`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-restart-recovery-contract`

### Unit 3: Make Machine Remove Consume Serving Writer

Files:

- `MVP/machine/src/remove.rs`
- `MVP/machine/src/error.rs`
- `MVP/machine/src/lib.rs`
- `MVP/machine/src/wire.rs`
- `MVP/e2e/src/machine_remove_contract.rs`

Work:

- Add a serving-writer generic to `MachineRemoveCoordinator`.
- Default the coordinator to routing's `BusServingFactWriter`.
- Replace direct `mvp_routing::write_serving_commit` calls with the injected
  writer.
- Keep the existing machine remove E2E source mix: iroh-docs for joined,
  removal-started, and tombstone facts; bus source for serving commits.
- Preserve the two explicit phases:
  `execute_until_serving_commit` and `finish_cleanup`.
- Keep validation and prepare-probe behavior before any durable mutation.

Test scenarios:

- Missing/tombstoned/already-removing/invalid target still fails before any
  fact write.
- No prepare responder still fails before removal-started.
- Prepare rejection writes removal-started but no serving commit or tombstone.
- Serving commit failure leaves removal-started intent only.
- Projection mismatch returns cleanup pending and writes no tombstone.
- Successful remove writes removal-started, serving commit, stop, tombstone in
  order.
- `machine-remove-contract` still proves all existing report booleans.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-machine`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract`

### Unit 4: Documentation And Next-Slice Ledger

Files:

- `MVP/slice-027-routing-owned-serving-commit.md`
- `MVP/overall-plan.md`
- `MVP/e2e-proof-plan.md`
- `MVP/primitive-decisions.md`

Work:

- Record that serving commit writer ownership moved to routing.
- Record that p2panda machine remove was deliberately split out.
- Report LOC by category:
  business/domain, adapter/backend, shared foundation, tests, docs.
- State explicitly that `PhasedCommand` remains deferred after this slice.
- Add the next-slice trigger: p2panda machine remove canary must first decide
  how joined-node facts enter the p2panda projection input and how
  `PandaFactError` maps into `MachineRemoveError`.

Verification:

- Docs use repo-relative paths.
- `git diff --check`

## Review Risks

- Accidentally making `mvp-routing` depend upward on deploy or machine.
- Moving the serving writer trait but leaving duplicate deploy-owned aliases
  that keep two canonical owners alive.
- Letting p2panda adapter crates make trust/authority decisions instead of
  receiving explicit sessions/authors.
- Hiding projection catch-up or stop/tombstone ordering behind a convenience
  helper.
- Expanding a narrow p2panda writer adapter into a broad store facade.
- Pulling p2panda machine remove or `PhasedCommand` forward before their
  decisions are explicit.

Review should include correctness, maintainability, project standards,
data-integrity/fact-boundary checks, and simplicity.

## Verification Gate

Targeted:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-routing
cargo test --manifest-path MVP/Cargo.toml -p mvp-routing-p2panda
cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy
cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy-p2panda
cargo test --manifest-path MVP/Cargo.toml -p mvp-machine
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-restart-recovery-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-routing -p mvp-routing-p2panda -p mvp-deploy -p mvp-deploy-p2panda -p mvp-machine -p mvp-e2e --all-targets -- -D warnings
```

Closeout:

```bash
MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
git diff --check
```

## Done Criteria

- Serving fact writer ownership has one home: `mvp-routing`.
- Deploy and machine remove consume the routing-owned serving writer contract.
- p2panda serving writes live in a routing adapter edge, not deploy.
- Machine remove still proves route cutover before stop and tombstone after
  stop.
- Core command crates remain substrate-free.
- p2panda machine-remove conversion is not hidden in this slice; its open
  decisions are recorded for the next plan.
- The slice ledger reports whether shared foundation LOC increased and why.
