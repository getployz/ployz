---
title: Slice 043 Membership-backed Machine Remove Plan
status: active
created: 2026-05-19
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-041-p2panda-auth-membership-substitution.md
  - MVP/slice-042-membership-backed-acme-sync.md
external:
  - https://docs.rs/p2panda-auth
  - https://docs.rs/p2panda-sync/latest/p2panda_sync/log_sync/
---

# Slice 043 Membership-backed Machine Remove Plan

## Problem Frame

Slice 041 made durable p2panda-auth membership the fact-store authority source.
Slice 042 moved ACME and the main p2panda sync proof onto that shape. The next
remaining product canary with meaningful manual trust is
`machine-remove-contract`: it still seeds p2panda author keys and replica peers
manually when rebuilding stores for recovery and completed replay.

Machine remove is the right next canary because it is not just a fact-source
demo. It covers command intent, serving cutover, tombstone, cleanup recovery,
projection rebuild, and WireGuard/process-role behavior. Moving it to
membership-backed authority proves the new authority shape holds for a
multi-writer, multi-stage product command, not only ACME/sync.

This is a deletion/containment slice. It should not change machine-remove
business behavior, add active-partition checks, add quorum, or introduce a new
authority abstraction.

## Dependency Scout

Checked on 2026-05-19:

- `p2panda-auth` still exposes the access model this MVP maps onto island
  membership: `Pull`, `Read`, `Write`, `Manage`, manager-only group mutation,
  and strong-removal-oriented conflict resolution. Ployz should continue to
  consume this through `IslandAuthzStore` and `IslandAuthoritySnapshot`, not by
  leaking p2panda-auth generic identities into product code.
- `p2panda-sync` log sync remains a generic append-log protocol. The machine
  remove contract currently replays exported operations deterministically rather
  than running live sync; membership-backed import authority is the target here,
  not a transport migration.

## Scope

In scope:

- Build machine-remove E2E facts from a durable p2panda-auth membership store.
- Authorize the join writer, machine-remove writer, routing writer, and replica
  importer through membership snapshots.
- Open rebuilt/replayed in-memory machine fact stores by installing the
  `IslandAuthoritySnapshot` produced by the shared membership fixture, instead
  of `trust_author_key` and `trust_replica_peer`. `PandaFactAuthoritySource`
  remains the SQLite/open-config path; do not invent a new machine-store API for
  this slice.
- Keep Ployz fact-key grants explicit. Membership says a principal may be an
  island writer or replica importer; bus grants still decide which fact keys
  the principal may write.
- Preserve all current `machine-remove-contract` behavior and metrics:
  serving cutover before stop, cleanup recovery, no precommit replay,
  tombstone ordering, WireGuard peer removal, and no fresh rebuild conflicts.
- Record any remaining manual trust outside machine remove as follow-up, not as
  part of this slice.

Out of scope:

- No production machine invite or p2panda-auth membership replication over the
  network.
- No change to machine remove command semantics, visible-node consistency, or
  tombstone/reinvite policy.
- No broad migration of deploy, volume, or p2panda-net fallback probes.
- No removal of `PandaMachineFactStore::trust_author_key` or
  `trust_replica_peer` yet if other fixtures still call them.
- No edits outside `MVP/`.

## Non-Negotiable Landing Gates

- `machine-remove-contract` must not call `trust_author_key` or
  `trust_replica_peer`.
- Rebuilt recovery stores must import exported operations through a
  membership-authorized replica session.
- The replica importer must not be treated as a writer.
- A machine writer with membership but without the required fact-key grant must
  still be rejected. This is the one required negative assertion for the slice;
  do not expand into a general auth matrix.
- Existing machine-remove E2E metrics must remain behaviorally equivalent.
- `mvp-e2e -- all` must continue to include and pass `machine-remove-contract`.

## Implementation Units

### Unit 1: Machine Remove Membership Fixture

Files:

- `MVP/e2e/src/machine_remove_contract.rs`
- `MVP/e2e/src/p2panda_projection_fixture.rs`

Plan:

1. Instantiate the shared p2panda membership fixture for:
   - `join-writer` as a writer,
   - `machine-remove-writer` as a writer,
   - `routing-writer` as a writer,
   - `machine-remove-replica` as a replica importer.
2. Reuse or lightly extend `P2pandaMembershipFixture` so the machine-remove
   contract can obtain an `IslandAuthoritySnapshot` without local root setup.
3. Open every in-memory `PandaMachineFactStore` in this contract with that
   membership snapshot installed. The known openings are the primary store,
   rebuilt recovery store, completed replay store, fresh rebuild store, and
   duplicate tombstone probe store.
4. Keep session grants exactly scoped to the existing fact-key patterns.

Test Scenarios:

- Initial joined-node facts and serving facts still write through the existing
  scoped writer sessions.
- Projection can still build the initial machine-remove state from the primary
  membership-backed store.
- No `trust_author_key` or `trust_replica_peer` remains in
  `machine_remove_contract.rs`.

Verification:

```text
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
rg -n "trust_author_key|trust_replica_peer|with_trusted_author_key|from_trusted_authors" MVP/e2e/src/machine_remove_contract.rs
```

### Unit 2: Recovery And Completed Replay Import Through Membership

Files:

- `MVP/e2e/src/machine_remove_contract.rs`

Plan:

1. Replace recovery-store manual author/replica trust with an open helper that
   installs `membership.authority_snapshot(...).await?` from the machine-remove
   membership fixture.
2. Import exported pre-cleanup operations through the membership-authorized
   replica session.
3. Do the same for the completed replay store used after cleanup.
4. Keep deterministic exported-operation replay as the existing recovery
   harness exception. Do not broaden the slice into p2panda-sync or iroh
   transport migration; record that as follow-up if it becomes blocking.

Test Scenarios:

- Recovery reads pending cleanup from the imported membership-backed store.
- Recovery still does not rerun probe/prepare/pre-serving writes.
- Cleanup writes tombstone and cleanup-done in the same order as before.
- Completed replay can import all completed operations and project a clean
  state with zero fresh rebuild conflicts.

Verification:

```text
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
```

### Unit 3: Negative Authority Coverage And Containment Note

Files:

- `MVP/e2e/src/machine_remove_contract.rs`
- `MVP/design-notes/semantic-leverage-loc.md`
- `MVP/slice-043-membership-backed-machine-remove-plan.md`
- `MVP/overall-plan.md`

Plan:

1. Add exactly one migration-critical negative check: a member with island
   writer membership but without the relevant fact-key grant cannot write
   machine-remove facts. Do not add new cross-island, non-member, or removed-key
   scenarios here.
2. Re-run the containment grep and classify remaining manual trust:

```text
cd MVP && rg "p2panda-trusted-author|p2panda-author|p2panda-author-key|TrustedP2pandaAuthor|trusted_author_keys|trusted_replica_peers|trust_replica_peer|trust_author_key" \
  p2panda-authz p2panda-facts p2panda-transport e2e/src
```

3. Record a short completion note that machine remove moved off manual trust.
   Keep broader remaining-inventory work to the existing roadmap; do not churn
   `primitive-decisions.md` or `e2e-proof-plan.md` unless the implementation
   changes an actual decision.
4. Update the semantic-leverage LOC ledger with whether this deleted local setup
   or only shifted authority semantics, because `overall-plan.md` requires
   slice closeouts to preserve that evidence.

Test Scenarios:

- Containment grep has no machine-remove hits for manual trust.
- `machine-remove-contract` remains in `mvp-e2e -- all`.
- Any added negative check returns a structured `PandaFactError`, not a parsed
  display string.

Verification:

```text
cargo check --manifest-path MVP/Cargo.toml -p mvp-e2e
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
git diff --check
```

## Review Focus

Run review subagents after implementation for:

- correctness: recovery imports must actually use membership-backed authority,
  not a still-trusted fallback;
- security/authorization: membership, replica import, and fact-key grants must
  remain separate;
- simplicity: the slice should reduce or centralize setup, not create another
  machine-remove-only authority helper if the shared fixture can do it.

## Completion Evidence

The slice is complete when:

- `machine-remove-contract` passes on membership-backed stores;
- `mvp-e2e -- all` passes with the contract included;
- targeted grep shows no manual-trust calls in
  `MVP/e2e/src/machine_remove_contract.rs`;
- the slice plan/completion note records the recovery-harness replay exception
  and the transport migration follow-up;
- semantic-leverage/LOC evidence is updated for the machine-remove migration;
- changes are reviewed, simplified, committed, and pushed on
  `feat/iroh-bus-mvp-foundation-clean`.
