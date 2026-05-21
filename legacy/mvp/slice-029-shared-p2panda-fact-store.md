---
title: Slice 029 Shared p2panda Fact Store
status: completed
created: 2026-05-18
plan: MVP/slice-029-shared-p2panda-fact-store-plan.md
---

# Slice 029 Shared p2panda Fact Store

## What Changed

Slice 029 centralizes the repeated cloneable p2panda fact-store wrapper in
`mvp_p2panda_facts::SharedPandaFactStore`.

The shared handle now owns:

- async key/payload writes,
- operation export,
- direct author-key import,
- trusted replica import,
- trusted author and replica registration,
- synchronous non-blocking write preflight,
- `FactSource` delegation with the same stale-candidate filtering as
  `PandaFactStore`.

The slice also deletes the routing-owned generic p2panda sink trait. Routing's
serving writer now depends directly on `SharedPandaFactStore`; deploy and
machine adapters no longer depend on `mvp-routing-p2panda` in production.

## Preserved Boundaries

Domain writers stayed domain-specific:

- `PandaDeployFactWriter` still maps p2panda outcomes into deploy facts and
  deploy errors.
- `PandaMachineFactWriter` still maps p2panda authorization/conflict failures
  into branchable machine-remove errors.
- `PandaServingFactWriter` still maps serving fact conflicts into routing
  errors.
- The volume transfer E2E fixture still owns lease/ownership metrics and
  volume-specific outcome conversion.

Replay modes stayed distinct:

- deploy restart recovery preserves direct author-key import,
- machine remove preserves trusted replica import,
- volume transfer recovery preserves direct author-key import.

## Semantic Leverage

Implementation diff for the slice files:

- `MVP/p2panda-facts/src/lib.rs`: shared substrate added once.
- `MVP/deploy-p2panda/src/lib.rs`: removed the deploy-local shared-store shell.
- `MVP/machine-p2panda/src/lib.rs`: removed the machine-local shared-store
  shell.
- `MVP/routing-p2panda/src/lib.rs`: removed the generic sink trait and its test
  fixture wrapper.
- `MVP/e2e/src/volume_transfer_contract.rs`: removed the E2E-local raw
  `Arc<Mutex<PandaFactStore>>` wrapper mechanics while keeping volume-specific
  behavior local.

Net implementation movement across touched slice files: 382 insertions and 323
deletions. Most insertions are shared substrate tests; production adapter LOC
shrank in deploy, machine, and routing.

Current line counts after the slice:

- `MVP/p2panda-facts/src/lib.rs`: 3,192 LOC.
- `MVP/routing-p2panda/src/lib.rs`: 184 LOC.
- `MVP/deploy-p2panda/src/lib.rs`: 333 LOC.
- `MVP/machine-p2panda/src/lib.rs`: 549 LOC.
- `MVP/e2e/src/volume_transfer_contract.rs`: 1,164 LOC.

## Verification

Passed:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-routing-p2panda`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy-p2panda`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-machine-p2panda`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-restart-recovery-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- volume-transfer-contract`

Closeout clippy and full E2E remained for the final verification pass when this
report was written.
