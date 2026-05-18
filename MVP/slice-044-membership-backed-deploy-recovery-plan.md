---
title: Slice 044 Membership-backed Deploy Recovery Plan
status: active
created: 2026-05-19
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/semantic-leverage-loc.md
  - MVP/slice-041-p2panda-auth-membership-substitution.md
  - MVP/slice-042-membership-backed-acme-sync.md
  - MVP/slice-043-membership-backed-machine-remove-plan.md
external:
  - https://docs.rs/p2panda-auth
  - https://docs.rs/p2panda-net
  - https://docs.rs/p2panda-sync/latest/p2panda_sync/log_sync/
---

# Slice 044 Membership-backed Deploy Recovery Plan

## Problem Frame

Slice 043 moved `machine-remove-contract` off manual p2panda author and replica
trust. The remaining manual-trust inventory includes product-shaped deploy
restart recovery and volume transfer, plus the explicitly low-level
p2panda-net fallback fixture. Deploy is the right next slice because it is the
central product invariant from the architecture map: route cutover is a durable
fact, and drain is a consequence of that fact.

`deploy-restart-recovery-contract` currently proves the restart invariant, but
its recovery store imports exported operations by manually trusting the
operator author key. That keeps a second authority shape alive in one of the
most important product canaries. This slice should move deploy restart recovery
onto the same membership-backed writer/replica-importer model as ACME, sync,
and machine remove, without changing deploy business behavior.

This is a targeted authority-containment slice. It should not redesign deploy,
introduce quorum, add active-partition membership, or port the old
`crates/ployzd/src/daemon/deploy.rs` shape forward.

## Why This Before Volume Or p2panda-net Fallback

Deploy recovery has the best next payoff:

- It is product-critical and already small enough to migrate without broad
  command refactoring.
- It exercises deploy decision, serving commit, projection catch-up, cleanup
  recovery, and last-good serving during coordinator outage.
- It has one obvious manual-trust recovery path, so the slice can stay narrow.

Volume transfer is a strong follow-up, but it has more repeated E2E-local store
setup and may deserve a separate decision about whether to extract a
`mvp-volume-p2panda` adapter. The p2panda-net fact-node contract is currently a
low-level negative regression fixture; polishing that before deploy would move
less product risk.

## Dependency Scout

Checked on 2026-05-19:

- `p2panda-auth` remains the right membership primitive for this slice. It
  exposes per-member `Pull`/`Read`/`Write`/`Manage` access, strict
  manager-only group mutation, eventually consistent group state, and strong
  removal defaults. Ployz should keep consuming it through
  `IslandAuthoritySnapshot`, not by leaking p2panda-auth generics into deploy.
- `p2panda-sync` and `p2panda-net` remain transport/sync references. This
  slice does deterministic exported-operation replay for crash recovery, so the
  target is membership-backed import authority, not live network transport.
- No new crate is justified. The existing shared E2E membership fixture and
  `SharedPandaFactStore::install_authority_snapshot` are the right seam.

## Scope

In scope:

- Open the deploy restart recovery E2E's primary and recovered p2panda fact
  stores from a durable p2panda-auth membership snapshot.
- Authorize the deploy/serving writer as a membership writer and the recovery
  importer as a membership replica importer.
- Replace recovery import's `trust_author_key` plus direct author import with
  membership-authorized replica import.
- Keep Ployz fact-key grants explicit for deploy and serving facts.
- Preserve all current `deploy-restart-recovery-contract` behavior and metrics:
  no drain before projection proof, serving answers while the coordinator is
  absent, recovery does not rerun precommit work, cleanup-pending is visible
  when stop has no responder, and cleanup-done makes recovery idempotent.
- Add narrow negative authority probes around deploy recovery import.
- Leave `mvp-deploy-p2panda` adapter unit tests on raw in-memory stores. Those
  tests validate write/conflict mapping, not membership behavior; run them as
  regression gates, but do not rewrite them in this slice.
- Update the semantic-leverage ledger and roadmap with what got simpler and
  what manual-trust inventory remains.

Out of scope:

- No deploy state-machine rewrite and no migration of deploy to
  `PhasedCommand`.
- No new deploy command phases, participant ABI changes, route semantics, or
  cleanup policies.
- No p2panda-net deploy replication or iroh transport migration.
- No volume transfer migration.
- No churn to environment command phase conflict tests that intentionally use a
  narrow raw-store fixture.
- No broad deletion of manual trust APIs from `mvp-p2panda-facts`; low-level
  tests may still exercise fallback behavior.
- No edits outside `MVP/`.

## Non-Negotiable Landing Gates

- `deploy-restart-recovery-contract` must not call `trust_author_key`,
  `trust_replica_peer`, `with_trusted_author_key`, or
  `PandaFactSyncScope::from_trusted_authors`.
- Product-shaped deploy stores in this E2E must be opened through one
  membership-backed helper. Any raw `PandaFactStore::new` in the file must be
  either inside that helper or inside an explicitly named negative fixture.
- Recovery import must use a membership-authorized replica importer, not the
  original writer session.
- A same-island non-replica principal must be unable to import an otherwise
  valid deploy or serving operation. The test must catch an implementation that
  accidentally uses `import_operation` instead of `import_replica_operation`.
- The replica importer must not become a writer, even if its bus grant would
  otherwise allow deploy or serving fact keys.
- Recovery import must still validate the original operation author's
  membership and Ployz fact-key grant.
- Recovery import must reject a foreign-island deploy operation.
- Use stable `PandaFactAuthor` values. The p2panda-auth membership snapshot
  binds `(island, principal, epoch, author key)`, so recreating an author after
  fixture creation with a different key would correctly fail.
- The existing deploy restart report fields and behavioral assertions remain
  meaningful. In particular, `coordinator_outage_ms` must not include extra
  negative-probe work.
- `mvp-e2e -- all` must continue to include and pass
  `deploy-restart-recovery-contract`.

## Implementation Units

### Unit 1: Deploy Recovery Membership Fixture

Files:

- `MVP/e2e/src/deploy_restart_recovery_contract.rs`
- `MVP/e2e/src/p2panda_projection_fixture.rs`

Plan:

1. Instantiate the shared p2panda membership fixture for:
   - deploy/serving writer principal as a writer,
   - recovery replica principal as a replica importer,
   - any denied-source authors needed by negative import probes.
2. Add one local opener such as `open_membership_deploy_store(...)` that wraps
   `PandaFactStore::new`, installs `membership.authority_snapshot(island)`, and
   returns `SharedPandaFactStore`.
3. Use the opener for the primary deploy fact store and recovered fact store.
4. Keep existing bus grants narrow enough to prove Ployz fact-key permissions:
   deploy writer grants cover `/facts/deploy/>` and `/facts/serving/>`;
   projection still has read-only access; the normal recovery replica importer
   has no bus write grant.
5. Add a separate replica-write probe session whose bus grant allows deploy or
   serving fact writes while its p2panda-auth role is replica importer only.
   This proves membership-layer writer rejection, not simple bus denial.
6. Avoid creating a deploy-specific authority abstraction. The E2E can have a
   small fixture struct for sessions/authors if that makes the contract easier
   to read.

Test Scenarios:

- Deploy decision, serving commit, and cleanup-done facts are written by a
  membership-backed writer.
- Projection still rebuilds serving state from the primary store.
- Serving answers while the coordinator is absent.
- A replica-importer-only principal with a temporary bus write grant for
  `/facts/deploy/>` and `/facts/serving/>` still cannot directly write deploy
  or serving facts. This must prove membership-layer writer rejection, not a
  missing bus grant.
- Adjacent deploy E2Es still pass, proving the authority migration did not
  perturb commit-before-drain or pre-serving cleanup behavior.

Verification:

```text
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-restart-recovery-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-commit-drain-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-candidate-cleanup-contract
! rg -n "trust_author_key|trust_replica_peer|with_trusted_author_key|from_trusted_authors" MVP/e2e/src/deploy_restart_recovery_contract.rs
rg -n "PandaFactStore::new" MVP/e2e/src/deploy_restart_recovery_contract.rs
```

### Unit 2: Recovery Import Uses Replica Membership

Files:

- `MVP/e2e/src/deploy_restart_recovery_contract.rs`

Plan:

1. Replace `import_panda_deploy_facts(...)` with a membership-backed import
   helper that opens the recovered store through the shared membership snapshot.
2. Import exported operations with `SharedPandaFactStore::import_replica_operation`
   and a recovery replica session.
3. Keep deterministic exported-operation replay as the existing crash-recovery
   harness exception. This slice proves authority on replay; it does not
   convert replay to live p2panda-net transport.
4. Add a same-island negative source operation whose author is a membership
   writer in both source and target membership, but whose target-side session
   lacks the deploy or serving fact-key grant. Use separate source and target
   bus authorizers: the source authorizer may grant `/facts/deploy/>` or
   `/facts/serving/>` so the operation can be signed and exported; the recovered
   target authorizer must reject that imported operation with a structured
   fact-key authorization error.
5. Split the fact-key denial probe in two:
   - source author has only `/facts/serving/>` on the target and imports a
     deploy decision or cleanup fact, which must be rejected;
   - source author has only `/facts/deploy/>` on the target and imports a
     serving commit fact, which must be rejected.
6. Add a same-island original-author membership probe: an operation signed by an
   author with a bus fact grant but no active writer membership must be rejected
   with a structured membership/key error. If the source operation needs to be
   manufactured through an explicitly named raw negative fixture, keep that raw
   store outside the product recovery path and name it as a negative fixture.
7. Add a non-replica import probe: a same-island principal with the original
   author and fact-key grants but no replica-import membership attempts to
   import an otherwise valid deploy/serving operation and must fail with the
   structured unauthorized-replica-import error.
8. Add a foreign-island negative source operation and assert recovery import
   rejects it with the structured island-mismatch error.
9. Keep the existing recovery assertions unchanged: no precommit rerun, pending
   cleanup recovery, cleanup-pending when stop has no responders, final stop
   completion, and cleanup-done idempotency.

Test Scenarios:

- Recovery reads pending cleanup from the membership-backed recovered store.
- Recovery does not rerun capacity, prepare, start, or serving writes.
- A valid local replica importer cannot import a deploy operation whose
  original author lacks the required fact-key grant.
- A valid local replica importer cannot import a serving operation whose
  original author lacks the required serving fact-key grant.
- A valid local replica importer cannot import a same-island operation whose
  original author has a bus fact grant but no active writer membership.
- A same-island non-replica principal cannot import an otherwise valid operation.
- A valid local replica importer cannot import a foreign-island deploy
  operation.
- Completed cleanup remains idempotent after the final cleanup-done fact.

Verification:

```text
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-restart-recovery-contract
cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy-p2panda
cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy
```

### Unit 3: Containment Ledger And Full Gate

Files:

- `MVP/design-notes/semantic-leverage-loc.md`
- `MVP/overall-plan.md`
- `MVP/slice-044-membership-backed-deploy-recovery-plan.md`

Plan:

1. Record that deploy restart recovery moved to membership-backed authority and
   name remaining manual-trust inventory, especially volume transfer and
   p2panda-net fallback probes.
2. Keep the p2panda-net fact-node contract classified as a low-level fallback
   regression fixture. Do not implement a p2panda-net fallback replacement in
   this slice.
3. Add a small semantic-leverage note: this slice should ideally remove a
   deploy-specific trust setup path without adding shared substrate. If it grows
   E2E lines, explain why the product authority model is still simpler for
   future deploy work.
4. Keep plan, implementation, review fix, and simplify commits separate if the
   implementation expands beyond a narrow E2E edit.

Test Scenarios:

- Targeted manual-trust grep has no hits in
  every file touched by the slice, at minimum
  `MVP/e2e/src/deploy_restart_recovery_contract.rs` and
  `MVP/e2e/src/p2panda_projection_fixture.rs`.
- The raw-store grep has only the membership-backed opener and any explicitly
  named negative fixture.
- The recovery helper visibly calls `import_replica_operation`; targeted grep
  should not show `import_operation(` in the deploy restart contract except in
  explicitly named negative probes.
- `deploy-restart-recovery-contract` remains part of `mvp-e2e -- all`.

Verification:

```text
cargo fmt --check --manifest-path MVP/Cargo.toml --all
cargo check --manifest-path MVP/Cargo.toml -p mvp-e2e
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-restart-recovery-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-commit-drain-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-candidate-cleanup-contract
cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy-p2panda
cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
git diff --check
```

## Review Focus

Run review subagents after implementation for:

- correctness: recovery must really import through replica membership, and
  deploy restart semantics must not change;
- security/authorization: membership writer, replica importer, and Ployz
  fact-key grants must remain distinct;
- maintainability: the slice should centralize deploy E2E authority setup
  without growing a second membership fixture;
- test coverage: negative probes should assert structured error variants, not
  display strings.

## Expected Follow-up

If this slice lands cleanly, the next manual-trust target should probably be
volume transfer. It has more repeated raw p2panda store setup and may be the
point where a tiny `mvp-volume-p2panda` adapter becomes worthwhile. That
decision should be made in its own plan, not folded into deploy recovery.
