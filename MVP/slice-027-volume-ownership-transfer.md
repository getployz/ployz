---
title: Slice 027 Volume Ownership Transfer
status: completed
completed: 2026-05-18
plan: MVP/slice-027-volume-ownership-transfer-plan.md
---

# Slice 027 Volume Ownership Transfer

Slice 027 adds the first MVP volume movement canary. It does not implement ZFS
send/receive or a reusable volume p2panda adapter. It proves the command shape:
read current ownership and lease candidates, write a durable advisory lease
claim, request snapshot/receive through the bus, validate exact receive
evidence, and only then commit the new owner as a durable p2panda-backed fact.

## Proof Added

- New crate: `mvp-volume`.
- New E2E: `volume-transfer-contract`.
- New scenario registration in `mvp-e2e -- all`.

The E2E proves:

- active lease conflict fails before participant RPC,
- ownership fact authorization is preflighted before lease write or participant
  RPC,
- a successful transfer writes the lease claim before snapshot/receive,
- ownership moves only through `/facts/volume/<namespace>/<volume>/ownership/<epoch>`,
- restart/recovery reads the committed owner from exported/imported p2panda
  operations without rerunning snapshot or receive,
- stale lease holders and expired leases cannot write the ownership commit after
  participant work,
- a concurrent ownership winner that appears during participant work rejects the
  stale command before commit,
- fact-write conflicts are foreground failures and the command verifies its
  written ownership fact is the reducer winner,
- superseded ownership candidates are retained in the read model,
- forged receive evidence is rejected before ownership commit,
- pre-commit drop leaves the old owner authoritative,
- results include visible nodes at decision time,
- source cleanup remains explicitly `Deferred` and non-authoritative.

Latest targeted run:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-volume
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- volume-transfer-contract
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-volume -p mvp-e2e --all-targets -- -D warnings
MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
```

## Semantic-Leverage Ledger

Old reference baseline:

- `crates/ployzd/src/daemon/handlers/deploy/volume_transfer.rs`: 226 LOC
- `crates/ployzd/src/daemon/handlers/volume/mod.rs`: 2 LOC
- `crates/ployzd/src/daemon/handlers/volume/transfer_listener.rs`: 1,104 LOC
- `crates/ployz-api/src/volume.rs`: 119 LOC
- `crates/ployz-e2e/src/scenarios/volume_clone_branch_real_smoke.rs`: 256 LOC
- Total reference surface: 1,707 LOC

MVP Slice 027 surface:

- Feature/domain crate: `MVP/volume/src/{domain,error,facts,wire,command,lib}.rs`
  is about 1,150 LOC.
- Product/domain tests: `MVP/volume/src/tests.rs` are about 800 LOC.
- E2E p2panda harness and contract: `MVP/e2e/src/volume_transfer_contract.rs`
  is about 1,200 LOC.
- Shared foundation LOC added: one workspace member and one E2E scenario entry.
- Public substrate added: no generic workflow runner, no ZFS backend, no shared
  projection/cache, no reusable p2panda volume adapter.

Assessment: **yellow-green**. This slice is not a raw LOC reduction against the
listed reference files. The business rule is explicit and mostly lives in one
command/reducer crate, but the E2E-local p2panda harness is still large. Do not
extract `mvp-volume-p2panda` from a single caller yet; the next similar
volume/storage command should decide whether the adapter repeats enough to earn
its own crate.

Maintenance read: the gain is semantic locality, not line count. The old
reference surface mixed listener mechanics, API shape, state inspection, and
branch smoke behavior. The MVP keeps the ownership rule in a typed
command/reducer and pushes transport/storage behind narrow traits. The burden to
watch is test harness size, not the command model.

## PhasedCommand Trigger

The trigger is close but not yet accepted. Deploy, ACME, and volume now all
show phased command pressure, but Slice 027 deliberately avoids durable
pre-commit phase facts. Plan `mvp-commands` when the next storage or membership
command needs three or more persisted phase boundaries or non-trivial
compensation. Until then, keep command logic explicit.
