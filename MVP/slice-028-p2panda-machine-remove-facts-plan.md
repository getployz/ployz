---
title: Slice 028 p2panda Machine Remove Facts Plan
status: active
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-017-graceful-machine-remove-plan.md
  - MVP/slice-017-graceful-machine-remove.md
  - MVP/slice-027-routing-owned-serving-commit.md
  - MVP/machine/src/remove.rs
  - MVP/e2e/src/machine_remove_contract.rs
external:
  - https://docs.rs/p2panda-net
  - https://docs.rs/p2panda-store
  - https://docs.rs/p2panda-sync
reviewed_by:
  - ce-feasibility-reviewer
  - ce-scope-guardian-reviewer
  - ce-data-integrity-guardian
---

# Slice 028 p2panda Machine Remove Facts Plan

## Problem Frame

Machine remove already has the right product invariant:

```text
removal_started -> target drains/no-new-work -> serving cutover -> projection catch-up -> stop -> tombstone
```

The current proof still writes machine facts through an iroh-docs-specific
E2E writer, while serving facts now have a routing-owned p2panda writer. That
keeps machine remove split across two fact substrates inside the same proof:
node join/removal/tombstone facts come from iroh-docs, serving commits come from
the bus/routing path, and the E2E has a `CombinedFactSource` solely to stitch
them back together.

Slice 028 should make machine remove use the same p2panda-backed fact boundary
as deploy, ACME, routing, and volume. This is not a rewrite of machine remove
semantics and not a generic machine-management framework. It is the narrow
storage-boundary canary called out after the routing-owned serving correction.

## Single Proof Target

Update `machine-remove-contract` so all durable facts needed by the proof enter
projection through one p2panda-backed fact source:

1. joined-node facts are produced by the existing `JoinCommand` and written to
   p2panda by a scoped join writer,
2. removal-started and tombstone facts are written by a p2panda-backed
   `MachineFactWriter`,
3. serving cutover is written by `mvp-routing-p2panda::PandaServingFactWriter`
   against the same store,
4. projection rebuilds from that one p2panda store,
5. graceful remove still proves cutover-before-stop and tombstone-after-stop,
6. exported p2panda operations imported into a fresh store reproduce the final
   removed-node projection.

If the old `machine-remove-contract` can be migrated directly, do that instead
of adding a second scenario. A parallel `p2panda-machine-remove-contract` is
acceptable only if direct migration makes the existing mesh/data-plane proof too
hard to keep readable in one slice.

## Requirements Trace

- `VISION.md`: machine remove is a north-star primitive. It must be explicit,
  foreground, retryable, and honest about what completed.
- `MVP/overall-plan.md`: after Slice 027, the next implementation/proof slice
  should pay down product semantic leverage by reusing bus, p2panda facts,
  projection, advisory leases, serving actors, or deploy adapters without
  growing foundational substrate.
- `MVP/primitive-decisions.md`: p2panda machine-remove facts were intentionally
  deferred until joined-node inputs and p2panda error mapping were planned.
- `MVP/slice-027-routing-owned-serving-commit.md`: machine remove now consumes
  routing's `ServingFactWriter`; this slice should use that p2panda serving
  adapter instead of reintroducing serving-write ownership in machine code.
- `MVP/e2e-proof-plan.md`: E2E-9 asks whether business rules become easier to
  express with fewer glue layers. This slice should reduce machine-remove E2E
  substrate glue, especially `DocsMachineFactWriter` and `CombinedFactSource`.

## Dependency Scout

Checked on 2026-05-18:

- `p2panda-net` already gives the maintained iroh/gossip/log-sync carrier used
  by the MVP transport path. No new transport crate is needed for this slice.
- `p2panda-store` is already wrapped by `mvp-p2panda-facts::PandaFactStore`.
  Machine remove should use that existing wrapper, not import p2panda store APIs
  directly.
- `p2panda-sync` is already surfaced through the existing sync/import boundary.
  This slice needs local p2panda write/rebuild proof; network sync is already
  covered by prior E2Es.
- No ZFS, WireGuard, workflow, or async-trait dependency should be added.

Decision:

- Add a small `mvp-machine-p2panda` adapter crate. The precedent from
  `mvp-deploy-p2panda` is strong enough now: keeping this E2E-local would
  preserve the adapter glue this slice is supposed to remove.
- Prefer migrating the existing `machine-remove-contract` to p2panda over
  adding a duplicate E2E.
- Do not extract a shared generic p2panda store handle in this slice unless the
  simplify pass shows the exact same wrapper has become unavoidable across
  `mvp-deploy-p2panda`, `mvp-routing-p2panda`, and the new machine adapter.

## Scope

In scope:

- p2panda-backed implementation of `MachineFactWriter`.
- p2panda-backed seeding of joined-node facts through the existing
  `JoinCommand` result.
- p2panda-backed serving commit write through `mvp-routing-p2panda`.
- One projection source for machine and serving facts in the machine-remove
  proof.
- Import/export recovery proof that a fresh p2panda store projects the final
  removed-node state.
- Structured mapping from p2panda write errors and conflicts into
  `MachineRemoveError`.
- Semantic-leverage accounting against the current machine-remove E2E glue.

Out of scope:

- Machine add/invite implementation changes.
- New membership protocol or WireGuard graph behavior.
- Generic `mvp-commands` or phase runner.
- Automatic cleanup resume after coordinator death before tombstone.
- Pending-remove recovery contracts or coordinator-resume after serving commit.
- New quorum, witness acks, or strict machine locks.
- Deleting the standalone iroh-docs contract.
- Changes outside `MVP/`.

## Key Decisions

### Joined Facts Stay Mesh-Owned

Do not introduce a `mvp-mesh-p2panda` writer just to seed this proof.
`JoinCommand::admit` already owns join validation and returns a fact key plus
payload. The E2E can write that output through `PandaFactStore` with a session
whose grant only allows `/facts/node/*/joined/>`.

This keeps the authority story explicit:

- join writer can write joined facts,
- machine-remove writer can write removal/tombstone facts,
- routing writer can write serving facts,
- projection reader can read `/facts/>`,
- no writer gets broad machine authority accidentally.

### Machine Remove Gets A Narrow p2panda Adapter

The adapter should be shaped like `mvp-deploy-p2panda`, but smaller:

- wrap `PandaFactStore` behind a cloneable store handle,
- implement `FactSource`,
- implement `mvp-routing-p2panda::PandaServingFactSink`,
- implement `MachineFactWriter` for removal-started and tombstone writes,
- export p2panda operations for recovery/rebuild proof.

Core `mvp-machine` must remain p2panda-free. If `MachineRemoveError` needs a new
variant, it should be generic machine fact storage language, not a p2panda type.

### Conflict Is Foreground Failure

Machine fact writes should map:

- `Inserted` and `AlreadyPresent` to success,
- `Conflict` to `MachineRemoveError::FactConflict`,
- p2panda backend failures to a structured machine fact write/store error.

Do not treat p2panda conflict as success. Machine remove changes routing and
stops workloads; a losing removal/tombstone candidate must be visible to the
caller.

For this proof, duplicate same-key/same-payload writes by a different authorized
machine-remove principal are accepted as idempotent cluster truth. That decision
must be named in tests. If that reads wrong during implementation, narrow the
proof to one machine-remove writer principal instead of silently changing the
meaning of `AlreadyPresent`.

### Tombstone Facts Are Not Cleanup Proof By Themselves

The coordinator is the thing that proves ordering. A raw
`/facts/node/<id>/tombstoned/<epoch>` fact in a rebuilt projection means the
node is tombstoned for scheduling and mesh purposes; it does not prove the stop
RPC or serving cutover happened. Slice 028 must prove tombstone acceptance
through the coordinator path and must not present a raw tombstone fact as
cleanup completion proof.

### No Coordinator-Resume Claim Yet

This slice may import final p2panda operations into a fresh store and rebuild
projection. It should not claim that a daemon can resume cleanup after a crash
between serving commit and tombstone. Any pending-remove recovery contract is a
separate future slice.

The current durable facts do not fully encode the original request's
`tombstone_epoch` unless the command constrains it to `removal_epoch + 1` or
persists it elsewhere. That is a real future design fork, not something to hide
inside a p2panda substitution slice. For Slice 028, constrain
`tombstone_epoch == removal_epoch + 1` in machine-remove preconditions, or add
an explicit test showing non-adjacent epochs are preserved only as raw fact
state and do not imply resumability.

## Implementation Units

### Unit 1: p2panda Machine Fact Adapter

Files:

- `MVP/Cargo.toml`
- `MVP/machine/Cargo.toml`
- `MVP/machine/src/error.rs`
- `MVP/machine-p2panda/Cargo.toml`
- `MVP/machine-p2panda/src/lib.rs`

Work:

- Add `mvp-machine-p2panda`.
- Define `PandaMachineFactStore`, a cloneable p2panda machine fact store
  wrapper.
- Define `PandaMachineFactWriter`, a p2panda-backed `MachineFactWriter`.
- Implement `FactSource` by delegating to `PandaFactStore`.
- Implement `PandaServingFactSink` so routing's p2panda serving writer can use
  the same store.
- Implement `MachineFactWriter` for removal-started and tombstone writes.
- Map p2panda write outcomes and backend errors into structured
  `MachineRemoveError` variants.
- Add or plan a machine-remove precondition for adjacent removal/tombstone
  epochs, unless implementation explicitly chooses the non-adjacent epoch test
  path described above.

Test scenarios:

- removal-started write returns the expected fact key,
- tombstone write returns the expected fact key,
- duplicate write is success,
- duplicate same-payload write by another authorized machine-remove principal is
  either named as accepted idempotency or rejected by single-writer authority,
- conflicting machine fact is `FactConflict`,
- unauthorized machine fact write is foreground failure,
- serving commit can be written through `PandaServingFactWriter` against the
  same store,
- adapter projects joined, removal-started, tombstone, and serving facts through
  `FactSource`.

Verification:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-machine -p mvp-machine-p2panda
```

### Unit 2: Migrate Machine Remove E2E To One p2panda Source

Files:

- `MVP/e2e/Cargo.toml`
- `MVP/e2e/src/machine_remove_contract.rs`
- `MVP/e2e-proof-plan.md`

Work:

- Replace `DocsMachineFactWriter` with the p2panda machine fact writer.
- Replace joined-node writes to `IrohFactDoc` with `JoinCommand` outputs written
  to the p2panda store by a scoped join writer.
- Replace `CombinedFactSource` with one p2panda fact source carrying node,
  machine, and serving facts.
- Write both `initial_serving_commit()` and `remove_serving_commit()` through
  `PandaServingFactWriter` into the same p2panda store before their respective
  projections.
- Add an E2E-local `RecordingMachineFactWriter<W>` wrapper that delegates to the
  p2panda writer and records `RemovalStarted`/`Tombstone` only after successful
  writes. Do not put E2E event logging inside `mvp-machine-p2panda`.
- Keep the existing participant RPC, route projection, WireGuard/data-plane,
  and event-order assertions intact.
- After cleanup, export operations, create a fresh p2panda store with equivalent
  authorizer grants, trust the join, machine-remove, and routing
  `PandaFactAuthorKey`s before import, import operations in exported order,
  rebuild projection with a `/facts/>` reader, and assert the target is
  tombstoned and excluded from live peers.

Test scenarios:

- initial joined-node projection is identical to the current proof,
- removal-started writes before route cutover,
- target is removed from active backends after serving commit,
- target stays in old backends until cleanup,
- stop is not attempted until projection catches up,
- tombstone writes only after stop succeeds,
- fresh-store import/rebuild preserves tombstone and serving cutover,
- fresh-store import/rebuild uses scoped trusted author keys for join,
  machine-remove, and routing authors,
- final fresh-store projection has zero joined/tombstone conflict candidates,
- an imported conflicting tombstone candidate does not silently project stale or
  ambiguous machine state,
- a raw tombstone fact alone is not reported as cleanup completion proof,
- join writer cannot write tombstone,
- machine writer cannot write joined-node facts,
- serving writer conflict is foreground failure.

Verification:

```bash
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
```

### Unit 3: Leverage Ledger And Decision Updates

Files:

- `MVP/slice-028-p2panda-machine-remove-facts.md`
- `MVP/overall-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/e2e-proof-plan.md`

Work:

- Record exactly which iroh-docs-specific machine-remove glue was removed.
- Record the new `mvp-machine-p2panda` boundary and why it is not a generic
  p2panda facade.
- Count old machine-remove E2E LOC before and after migration.
- Record any shared p2panda wrapper repetition discovered during the simplify
  pass, but do not extract it unless the implementation made the duplication
  concrete and costly.
- State clearly that cleanup resume after daemon death remains deferred to a
  later slice.

Verification:

```bash
git diff --check
```

## Review Risks

- Accidentally moving join validation out of `mvp-mesh`.
- Recreating a generic p2panda facade before the adapter repetition earns it.
- Keeping both iroh-docs and p2panda fact paths in the E2E, preserving the
  exact glue this slice exists to delete.
- Mapping p2panda conflicts into success.
- Giving the join writer or machine writer broad `/facts/node/>` authority.
- Claiming daemon-crash cleanup resume. It is explicitly out of scope here.
- Treating a raw tombstone fact as proof that serving cutover, projection
  catch-up, and stop completed.
- Breaking the steady-state data-plane proof while changing only storage
  mechanics.

Review should include feasibility, scope, correctness, maintainability,
data-integrity, project standards, and simplification.

## Verification Gate

Targeted:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-machine
cargo test --manifest-path MVP/Cargo.toml -p mvp-machine-p2panda
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-machine -p mvp-machine-p2panda -p mvp-e2e --all-targets -- -D warnings
```

Closeout:

```bash
MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
git diff --check
```

## Done Criteria

- `machine-remove-contract` no longer needs iroh-docs-specific machine fact
  writing or `CombinedFactSource` glue.
- Machine remove, joined-node inputs, and serving cutover project from one
  p2panda-backed fact source in the proof.
- Machine fact write conflicts fail loudly.
- Scoped grants prove join, machine-remove, and routing writers cannot mutate
  each other's fact namespaces.
- Final p2panda operations can be imported into a fresh store and rebuild the
  removed-node projection.
- Existing graceful remove ordering invariants remain intact.
- The slice report gives an honest LOC/maintenance-burden assessment.
