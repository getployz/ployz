---
title: Slice 043 Membership-backed Machine Remove Plan
status: completed
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
- `machine-remove-contract` must not open raw `PandaFactStore::new` stores on
  the product-shaped path. Every store in this contract must go through one
  membership-backed opener, or be explicitly named as a manual-fallback
  exception with rationale. The current expected result is no exceptions.
- Rebuilt recovery stores must import exported operations through a
  membership-authorized replica session.
- The replica importer must not be treated as a writer.
- Recovery import must still check the original operation author's fact-key
  grant, not only the replica importer's membership.
- Recovery import must reject operations from another island.
- A machine writer with membership but without the required fact-key grant must
  still be rejected without expanding into a general auth matrix.
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
3. Add one local `open_machine_facts_with_membership(...)` helper and use it for
   the primary store. Do not open raw `PandaFactStore::new` directly from the
   scenario body.
4. Keep session grants exactly scoped to the existing fact-key patterns.
5. Keep the replica importer session grant-free for writes and assert it cannot
   directly write a machine-remove fact.

Test Scenarios:

- Initial joined-node facts and serving facts still write through the existing
  scoped writer sessions.
- Projection can still build the initial machine-remove state from the primary
  membership-backed store.
- The replica importer can import through membership but cannot write
  machine-remove facts directly.
- No `trust_author_key` or `trust_replica_peer` remains in
  `machine_remove_contract.rs`.
- No bare `PandaFactStore::new` remains in `machine_remove_contract.rs` except
  inside `open_machine_facts_with_membership(...)`.

Verification:

```text
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
rg -n "trust_author_key|trust_replica_peer|with_trusted_author_key|from_trusted_authors" MVP/e2e/src/machine_remove_contract.rs
rg -n "PandaFactStore::new" MVP/e2e/src/machine_remove_contract.rs
```

### Unit 2: Recovery And Completed Replay Import Through Membership

Files:

- `MVP/e2e/src/machine_remove_contract.rs`

Plan:

1. Apply `open_machine_facts_with_membership(...)` to the rebuilt recovery store,
   completed replay store, fresh rebuild store, and duplicate tombstone probe
   store.
2. Import exported pre-cleanup operations through the membership-authorized
   replica session.
3. Do the same for the completed replay store used after cleanup.
4. Delete the `trust_fresh_store_authors` helper from the contract.
5. Add a targeted negative import probe showing that a valid replica importer
   cannot import an operation whose original author lacks the required
   machine-remove fact-key grant.
6. Keep deterministic exported-operation replay as the existing recovery
   harness exception. Do not broaden the slice into p2panda-sync or iroh
   transport migration. Record a transport follow-up only if implementation
   uncovers new blocking evidence.

Test Scenarios:

- Recovery reads pending cleanup from the imported membership-backed store.
- Recovery still does not rerun probe/prepare/pre-serving writes.
- Cleanup writes tombstone and cleanup-done in the same order as before.
- Completed replay can import all completed operations and project a clean
  state with zero fresh rebuild conflicts.
- A valid replica importer cannot smuggle a same-island operation from a member
  without the required fact-key grant into recovery.

Verification:

```text
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
cargo test --manifest-path MVP/Cargo.toml -p mvp-machine-p2panda
```

### Unit 3: Negative Authority Coverage And Containment Note

Files:

- `MVP/e2e/src/machine_remove_contract.rs`
- `MVP/design-notes/semantic-leverage-loc.md`
- `MVP/slice-043-membership-backed-machine-remove-plan.md`
- `MVP/overall-plan.md`

Plan:

1. Add the narrow negative coverage from Unit 1 and Unit 2 without broadening
   the scenario beyond machine-remove authority boundaries.
2. Run the targeted machine-remove containment grep:

```text
rg -n "trust_author_key|trust_replica_peer|with_trusted_author_key|from_trusted_authors" MVP/e2e/src/machine_remove_contract.rs
rg -n "PandaFactStore::new" MVP/e2e/src/machine_remove_contract.rs
```

The first grep must return no hits. The second grep must return only the
membership-backed opener. A broader manual-trust grep may be run as an
informational check, but do not classify, edit, or fail on non-machine-remove
call sites in this slice.

Optional inventory command:

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
- Negative checks cover direct replica writes and same-island import without the
  original author's fact-key grant. They should assert structured
  `PandaFactError` variants, not parse display strings.

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

## Implementation Result

Machine remove now opens all E2E-local in-memory machine fact stores with the
shared membership fixture's `IslandAuthoritySnapshot`: the primary store,
rebuilt recovery store, completed replay store, and conflicting tombstone probe
store. The node-only negative probe uses the primary membership-backed store.
The contract no longer calls
`trust_author_key` or `trust_replica_peer`.

The recovery harness still replays exported operations deterministically. That
is an explicit existing harness exception for command restart proof, not a new
transport pattern. A later transport slice can replace this with
`sync_panda_fact_stores` or p2panda-net/iroh transport when the product proof
requires it.

The new negative authority assertions cover the migration-critical boundaries:
a principal with island writer membership but without the command fact grant
cannot write machine-remove command facts; a replica importer with a
machine-remove fact grant is still not a writer; recovery import rejects an
operation whose original author lacks the command fact grant.

## Proofs

Targeted checks run during the slice:

```text
cargo check --manifest-path MVP/Cargo.toml -p mvp-e2e
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract
cargo test --manifest-path MVP/Cargo.toml -p mvp-machine-p2panda
cargo test --manifest-path MVP/Cargo.toml -p mvp-e2e --all-targets
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
git diff --check
```

Targeted manual-trust containment:

```text
rg -n "trust_author_key|trust_replica_peer|with_trusted_author_key|from_trusted_authors" \
  MVP/e2e/src/machine_remove_contract.rs
rg -n "PandaFactStore::new" MVP/e2e/src/machine_remove_contract.rs
```

The manual-trust grep has no hits in `machine_remove_contract.rs`. The raw
store-opening grep has one hit, inside `open_membership_machine_store`.

Broader remaining manual-trust hits are outside this slice: low-level
`p2panda-facts` fallback APIs/tests, `p2panda-transport` tests, deploy restart
recovery, p2panda-net fallback probes, and volume transfer.

## Semantic Leverage Ledger

Slice diff from the Slice 043 plan commit:

```text
MVP/e2e/src/machine_remove_contract.rs | 284 +++++++++++++++++++++++++--------
1 file changed, 216 insertions(+), 68 deletions(-)
```

This is a small net E2E increase, not a raw LOC deletion. The win is removing
another product canary's feature-local authority setup: machine remove now uses
the same p2panda-auth membership snapshot shape as ACME and sync, while keeping
fact-key grants as the command-specific authorization layer.
