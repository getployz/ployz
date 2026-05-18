---
title: Slice 027 Routing-Owned Serving Commit
status: completed
created: 2026-05-18
plan: MVP/slice-027-routing-owned-serving-commit-plan.md
---

# Slice 027 Routing-Owned Serving Commit

## Outcome

Serving commit writes now have one canonical owner: `mvp-routing`.

Deploy and machine remove both consume the routing-owned `ServingFactWriter`
contract. Deploy no longer defines serving-writer types, and machine remove no
longer writes serving commits directly to the bus. The p2panda serving writer
lives at the routing adapter edge in `mvp-routing-p2panda`, while
`mvp-deploy-p2panda` keeps only deploy-specific fact writing and recovery-read
logic.

This keeps the command shape explicit:

```text
machine remove:
  validate/probe
  write removal-started
  prepare target
  write serving commit through routing writer
  wait for projection catch-up
  stop workloads
  write tombstone
```

The slice deliberately did not convert machine-remove durable facts to
p2panda. That canary needs a separate decision about how joined-node facts
enter the p2panda projection input and how p2panda write errors map into
`MachineRemoveError`.

## What Changed

- Moved `WrittenServingFact`, `ServingFactWriteStatus`,
  `ServingFactWriter`, and `BusServingFactWriter` into
  `MVP/routing/src/lib.rs`.
- Deleted deploy-owned serving-writer ownership from `mvp-deploy`.
- Added `mvp-routing-p2panda` with a narrow `PandaServingFactWriter`.
- Made `PandaDeployFactStore` implement the narrow p2panda serving sink so the
  deploy restart E2E can share the same store shape without routing depending
  on deploy.
- Made `MachineRemoveCoordinator` generic over both machine fact writing and
  serving fact writing, with the default constructor using bus-backed writers.
- Updated `machine-remove-contract` to keep iroh-docs for machine facts and bus
  facts for serving commits, but route serving writes through the injected
  routing writer.

## LOC Ledger

Diff scope: implementation commits after
`MVP/slice-027-routing-owned-serving-commit-plan.md`, excluding the separate
volume plan commit and unrelated uncommitted volume work.

Total implementation diff:

```text
15 files changed, 527 insertions(+), 231 deletions(-)
```

Approximate category split:

| Category | Diff Shape | Notes |
| --- | ---: | --- |
| Business/domain | +115 / -33 | Machine remove coordinator injection plus deploy coordinator call-site cleanup. Some machine tests live in the same file. |
| Adapter/backend | +279 / -70 | New `mvp-routing-p2panda` adapter and deletion of deploy-owned serving adapter code. |
| Shared foundation | +112 / -115 | Routing gains the serving writer contract while deploy loses the duplicate owner. |
| Tests/E2E | +21 / -13 visible | E2E import/wiring changes plus deploy test import cleanup; machine unit-test additions are included in `remove.rs`. |
| Docs | this file plus ledger updates | No new generic substrate design was introduced. |

The maintenance result is a small but useful ownership win: feature code still
uses typed command writers, while route-cutover fact semantics live with
routing instead of deploy.

## Verification

Ran during the slice:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-routing -p mvp-deploy -p mvp-deploy-p2panda
cargo test --manifest-path MVP/Cargo.toml -p mvp-routing-p2panda
cargo test --manifest-path MVP/Cargo.toml -p mvp-machine
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-restart-recovery-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
```

The final all-run and clippy gate are recorded in the PR once closeout
verification completes.

## Deferred

- p2panda machine-remove fact writing.
- p2panda join-fact input for machine-remove projection.
- `PandaFactError` to `MachineRemoveError` mapping.
- `PhasedCommand`. This slice adds another command-shaped phase boundary, but
  it is still an ownership correction, not the right moment to add an
  orchestration primitive.
