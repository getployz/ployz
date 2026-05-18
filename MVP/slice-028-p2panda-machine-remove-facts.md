---
title: Slice 028 p2panda Machine Remove Facts
status: completed
completed: 2026-05-18
plan: MVP/slice-028-p2panda-machine-remove-facts-plan.md
---

# Slice 028 p2panda Machine Remove Facts

Slice 028 moves the machine-remove proof onto the same p2panda-backed fact
boundary used by deploy, routing, ACME, and volume. The graceful remove command
semantics did not change: removal-started is written before drain, serving
cutover happens before stop, projection catch-up gates cleanup, and tombstone is
written only after stop succeeds.

## Proof Added

- New crate: `mvp-machine-p2panda`.
- `machine-remove-contract` now seeds joined-node facts, machine remove facts,
  and serving commits into one `PandaMachineFactStore`.
- `PandaMachineFactWriter` implements `MachineFactWriter` for
  removal-started/tombstone writes.
- `PandaMachineFactStore` implements `FactSource` and
  `PandaServingFactSink`, so projection and routing use the same p2panda store.
- The E2E imports exported p2panda operations into a fresh store with trusted
  join, machine-remove, and routing author keys, then rebuilds the final
  removed-node projection.
- Adapter tests require trusted replica import and fail stale payload reads
  when a candidate changes between projection listing and payload loading.

The E2E proves:

- joined-node projection still matches the old proof,
- removal-started writes before route cutover,
- target is removed from active backends while kept in old backends until
  cleanup,
- stop is not attempted until projection catches up,
- tombstone writes only after stop succeeds,
- fresh-store rebuild has zero machine/serving conflict statuses,
- join writer cannot tombstone,
- machine-remove writer cannot write joined-node facts,
- conflicting tombstone candidates are not silently projected as a removal.
- fresh-store import uses a trusted replica principal rather than a read-only
  projection session.

Latest targeted run:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-machine -p mvp-machine-p2panda
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
```

## Semantic-Leverage Ledger

Before Slice 028:

- `MVP/e2e/src/machine_remove_contract.rs`: 934 LOC.
- E2E-local `DocsMachineFactWriter` wrote machine facts through iroh-docs.
- E2E-local `CombinedFactSource` stitched iroh-docs machine facts together with
  bus serving facts.

After Slice 028:

- `MVP/e2e/src/machine_remove_contract.rs`: 1,055 LOC.
- `MVP/machine-p2panda/src/lib.rs`: 749 LOC including adapter tests.
- `MVP/machine/src/error.rs`: structured p2panda fact-write denial variants
  plus a fallback `FactStore` variant.
- `MVP/machine/src/remove.rs`: cleanup failure classification now enumerates
  those new error variants.
- `DocsMachineFactWriter` and `CombinedFactSource` are gone.
- The E2E grew because it now proves scoped p2panda authority, fresh-store
  trust setup, and conflict rejection instead of only changing the storage
  backend.

Assessment: **yellow** on raw LOC, **green** on storage-boundary clarity. This
slice is not a line-count reduction. It removes the mixed iroh-docs/bus fact
path from machine remove and gives machine facts a reusable p2panda adapter, at
the cost of explicit adapter tests and stronger E2E authority checks.

Verified for this slice:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-machine -p mvp-machine-p2panda
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-machine -p mvp-machine-p2panda -p mvp-e2e --all-targets -- -D warnings
```

## Deferred

- Coordinator resume after serving commit remains out of scope. A raw tombstone
  fact means the node is excluded from scheduling/mesh projections; it is not
  proof by itself that serving cutover, projection catch-up, and stop completed.
- `mvp-machine-p2panda`, `mvp-deploy-p2panda`, and `mvp-routing-p2panda` now
  repeat a small cloneable p2panda store wrapper shape. Do not extract a generic
  p2panda facade yet; wait for one more command or the simplify pass to show
  the duplication is carrying real maintenance cost.
