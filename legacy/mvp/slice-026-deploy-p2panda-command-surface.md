---
title: Slice 026 Deploy p2panda Command Surface
status: completed
created: 2026-05-18
plan: MVP/slice-026-deploy-p2panda-command-surface-plan.md
---

# Slice 026 Deploy p2panda Command Surface

## Result

Slice 026 extracted the deploy-specific p2panda fact-writing and recovery-read
adapter from `MVP/e2e/src/deploy_restart_recovery_contract.rs` into a dedicated
`mvp-deploy-p2panda` crate.

The review-corrected boundary is important: `mvp-deploy` remains core-only and
has no p2panda dependency or feature flag. `mvp-deploy-p2panda` owns the
adapter edge:

- `PandaDeployFactStore` wraps `PandaFactStore`, implements `FactSource`, and
  exposes operation export for restart proof choreography.
- `PandaDeployFactWriter` writes deploy decision and cleanup-done facts.
- `PandaServingFactWriter` writes serving commit facts.
- p2panda write outcomes map to deploy-domain inserted, already-present, and
  structured conflict results.

The deploy restart-recovery E2E still owns the product proof: participant
fakes, serving/projection checks, timing metrics, process choreography, trusted
author setup, operation import, coordinator drop, recovery, and cleanup.

## Semantic Leverage

This is a maintenance-boundary win more than a raw LOC win.

- `deploy_restart_recovery_contract.rs` shrank from 945 to 789 lines.
- The deleted E2E-local p2panda deploy writer/outcome glue is now reusable.
- `mvp-deploy-p2panda/src/lib.rs` is 492 lines, including 277 lines of focused
  adapter tests.
- Core `mvp-deploy` stayed unchanged.

The broader read-only LOC check is directionally good for deploy: the
representative MVP deploy surface is much smaller than the old deploy surface.
That does not mean the whole MVP is cheap yet. Future slices should keep
tracking business/domain LOC, adapter/backend LOC, shared foundation LOC, test
LOC, and docs LOC so the foundation does not quietly become another large
system with too little business logic.

## What Stayed Deferred

- No `PhasedCommand` primitive yet. Deploy and ACME now show the pattern, but
  the trigger remains three or more command families with phase/resume logic.
- No p2panda-net deploy replication between real process roles.
- No coordinator redesign.
- No quorum, witness acks, strict leases, or consensus.
- No production runtime participant backend.

## Verification

Focused checks passed:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy-p2panda
cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy
cargo check --manifest-path MVP/Cargo.toml -p mvp-deploy --no-default-features -p mvp-e2e
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-restart-recovery-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-commit-drain-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-candidate-cleanup-contract
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-deploy -p mvp-deploy-p2panda -p mvp-e2e --all-targets -- -D warnings
```

Closeout checks passed:

```bash
MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
git diff --check
```
