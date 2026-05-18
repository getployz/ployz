---
title: Slice 041 p2panda-auth Membership Substitution Plan
status: completed
created: 2026-05-19
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution-audit.md
external:
  - https://docs.rs/p2panda-auth/0.6.0/p2panda_auth/
---

# Slice 041 p2panda-auth Membership Substitution Plan

## Problem Frame

Slice 040 deleted the opaque p2panda-net transport path. The next custom
substrate pressure is island membership and fact-import authority.

Today the MVP has two competing shapes:

1. `mvp-p2panda-authz` already wraps `p2panda-auth 0.6.0` group CRDTs,
   strong-removal membership reduction, signed membership operations, key
   bindings, role conditions, and `IslandAuthoritySnapshot`.
2. Product-shaped fact and serving paths still keep manual trust plumbing:
   `trusted_author_keys`, `trusted_replica_peers`,
   `--p2panda-trusted-author`, hand-built sync scopes, and process-role config
   that rebuilds authority from CLI flags instead of durable membership facts.

That second shape is the kind of code the MVP is supposed to delete early. It
will spread as soon as machine add, machine remove, ACME, deploy, and serving
all need "who can write/import this island?" answers.

This slice investigates and implements the highest-leverage substitution:
durable p2panda-auth-backed island membership becomes the normal authority
source for p2panda fact stores. Manual trust maps remain only as named test
fixtures or are deleted when no longer needed.

The adoption gate is deliberately stricter than "install a snapshot in memory."
The slice must prove:

```text
signed durable membership operations
  -> replayed p2panda-auth group state
  -> IslandAuthoritySnapshot
  -> PandaFactStore local write / replica import / sync scope checks
```

If the result still requires product-shaped paths to call
`trust_author_key`, `trust_replica_peer`, or pass general trusted-author CLI
flags, the substitution has not landed.

The slice may keep manual trust only in explicitly named fixture APIs and unit
tests. A product-shaped E2E or process role with a deferred manual-trust blocker
fails the slice.

## Dependency Scout

Checked from the active workspace on 2026-05-19:

- `p2panda-auth 0.6.0` is published on crates.io. It provides decentralized
  group management, `Pull`/`Read`/`Write`/`Manage` access levels, group
  operation DAG reduction, strict manager-only group modification, and the
  strong-removal resolver.
- `p2panda-auth 0.6.0` includes a `processor` feature with
  `GroupsProcessor` over `p2panda-store::SqliteStore`. That may simplify
  durable group-state processing, but the current MVP wrapper uses custom
  member ids, Ployz key-binding payloads, and Ployz-specific conditions, so the
  slice must verify whether the processor can replace local replay or whether
  it should remain a future migration target.
- `mvp-p2panda-authz` already uses `p2panda-auth 0.6.0` and
  `p2panda-store 0.6.0`.
- `PandaFactStore` already has the intended seam:
  `FactAuthority::Snapshot(IslandAuthoritySnapshot)` for authority-backed
  paths and `FactAuthority::Manual` for fallback trust maps. This slice should
  make product-shaped paths use the snapshot mode via durable replay, not add a
  third authority mode.
- The active p2panda-net path is already on non-RC `iroh 0.98.2` through
  `p2panda-net 0.6.0`. Do not add an RC iroh dependency. If any direct iroh
  work appears in this slice, use the stable line already resolved by p2panda
  or defer it.

Bias:

- Prefer deleting Ployz-owned membership plumbing in favor of p2panda-auth
  semantics, even if the p2panda-auth API is not yet v1-stable.
- Do not replace Ployz product policy. p2panda-auth owns membership group
  reduction. Ployz still owns root anchoring, principal/key/epoch binding,
  fact-key grants, subject permissions, command precondition conflicts, visible
  nodes at decision time, tombstone facts, and serving/deploy semantics.

## Scope

In scope:

- Deep API fit check for `p2panda-auth` processor/store support against the
  current custom `IslandAuthzMemoryLog`.
- Durable membership operation persistence and replay inside
  `mvp-p2panda-authz`.
- An authority-store/view API that can rebuild `IslandAuthoritySnapshot` from
  durable signed membership operations.
- Fact-store opening/import/rebuild paths that install authority from durable
  membership state instead of manual trusted-author maps on product-shaped
  paths.
- Process-serving and p2panda-net E2Es that carry membership state through the
  same process/restart boundary as facts.
- Tests for add, demote, remove, reinvite/new epoch, unauthorized replica
  import, stale removed writer rejection, and authority rebuild after restart.
- Tests that membership never bypasses Ployz fact-key authorization: an active
  p2panda-auth writer without the relevant Ployz fact grant is still denied.
- Documentation of exactly which manual trust paths are deleted, retained as
  fixtures, or gated for a later slice.

Out of scope:

- No RC iroh.
- No nested p2panda-auth group members.
- No quorum, witness acks, or active-partition membership checks. The
  operator's connected node remains the consistency boundary.
- No command blocking on global membership convergence. Command results still
  report visible nodes at decision time where relevant.
- No migration of PloyzBus subject grants, queue permissions, temporary reply
  permissions, or bridge import/export rules into p2panda-auth.
- No historical frontier/cutoff proof for accepting old post-removal facts. The
  authority-backed path should keep rejecting removed/demoted writer imports
  until a later proof can distinguish old legitimate operations from fresh
  stale-key operations.
- No replacement of machine membership, node tombstone projection semantics, or
  WireGuard peer planning with p2panda-auth island membership. The slice should
  document and test that these concepts do not contradict each other, but they
  remain separate primitives.
- No root workspace or existing `crates/` edits.

## Non-Negotiable Landing Gates

- `PandaFactStore` keeps only the existing authority split:
  `FactAuthority::Snapshot` for product paths and manual authority only for
  named fixtures. Do not add a third compatibility authority mode.
- Product-shaped paths must not call `trust_author_key`, `trust_replica_peer`,
  `PandaSqliteOpenConfig::with_trusted_author_key`, or general
  `--p2panda-trusted-author` flags by slice completion.
- Non-net serving trust flags are part of the same deletion pressure:
  `--p2panda-author`, `--p2panda-author-key`, and `TrustedP2pandaAuthor` must
  be audited and either replaced, renamed as publisher signing identity, or
  contained as fixtures.
- Membership is only coarse island writer/importer authority. Fact-key grants,
  subject grants, bridge grants, and read permissions still come from Ployz
  policy/bootstrap fixtures and must be checked after membership succeeds.
- Replica import checks two actors: the received fact operation's author must
  be an active writer, and the receiving session must be an active replica
  importer. One role must not imply the other.
- Root authority is anchored durably per island. Reopen/import must reject a
  competing root graph, a root mismatch, or a second root for the same island.
- Live membership replication is required only if it deletes the process-role
  manual-trust path. If canonical transport cannot carry membership operations
  without a new transport abstraction, stop at a durable bootstrap file for
  root/membership material and document membership net transport as the next
  slice.

## Implementation Units

### Unit 1: p2panda-auth Processor Fit Check

Files:

- `MVP/p2panda-authz/src/lib.rs`
- `MVP/p2panda-authz/Cargo.toml`
- `MVP/design-notes/p2panda-substitution-audit.md`
- `MVP/slice-041-p2panda-auth-membership-substitution.md`

Plan:

1. Build a tiny compile-backed spike inside `mvp-p2panda-authz` tests that
   exercises `p2panda_auth::processor::GroupsProcessor` with the same group
   action shape the MVP needs.
2. Compare it against the current wrapper's needs:
   - deterministic Ployz member ids,
   - introduced `(island, principal, epoch, author key)` binding,
   - Ployz `ReplicaImporter` condition,
   - root anchoring,
   - signed membership payload covering operation id, author, dependencies,
     group action, signer, and introduced binding.
3. Treat this as a stop/go gate before Unit 2. Choose one mapping:
   - adopt `GroupsProcessor` ids and document the new identity model, or
   - persist p2panda operations but replay with the current
     `GroupCrdt<AuthId, ...>` path.
4. Decide in the slice report whether the processor replaces local replay now
   or remains a future storage optimization.

Acceptance:

- The plan does not hand-wave "p2panda-auth can do this"; it names exactly
  which crate APIs are used and which Ployz-owned validation remains.
- Unit 2 does not start until the slice report names the chosen processor/store
  mapping.

### Unit 2: Durable Membership Store

Files:

- `MVP/p2panda-authz/src/lib.rs`
- `MVP/p2panda-authz/Cargo.toml`

Plan:

1. Introduce a durable authority store next to the current memory log. The
   target API should be small:

   ```text
   IslandAuthzStore::open(path, island, root_authority)
   create_root(...)
   apply_signed(...)
   add_writer(...)
   add_replica_importer(...)
   demote_to_replica_importer(...)
   remove_member(...)
   replay()
   authority_snapshot()
   export_operations()
   import_operation(...)
   ```

2. Store signed membership operations as validated p2panda operations with
   membership extensions, not ad hoc JSON rows. Operation identity should be
   derived from the durable signed p2panda operation or explicitly documented
   as a transitional local id with a deletion trigger.
3. Persist a canonical root anchor for the island and reject:
   - empty membership logs,
   - a second root for the same island,
   - a replay opened with the wrong root authority,
   - an imported membership graph rooted in a different root.
4. Rebuild authority by replaying durable operations after reopen.
5. Preserve branchable errors for wrong island, missing root, invalid
   signature, member key mismatch, duplicate operation, unauthorized manager,
   and malformed operation.
6. Keep `IslandAuthzMemoryLog` only as a fixture before Unit 3 begins. It must
   not be used by product-shaped E2Es after this slice.

Acceptance:

- Reopening the durable membership store reconstructs the same
  `IslandAuthoritySnapshot`.
- Duplicate membership operation import is idempotent.
- Out-of-order durable replay either converges through p2panda-auth ordering or
  returns a structured "dependency missing" style error. Do not silently drop.
- Shadow-root and root-mismatch attempts fail before changing the snapshot.

### Unit 3: Fact Store Authority Source

Files:

- `MVP/p2panda-facts/src/lib.rs`
- `MVP/p2panda-authz/src/lib.rs`
- `MVP/p2panda-transport/src/tests.rs`

Plan:

1. Add a narrow authority source object that opens/rebuilds from durable
   membership and yields `IslandAuthoritySnapshot` values for fact stores.
2. Add an epoch/dependency-aware `IslandMembershipView` behind that source.
   Authority-backed fact writes/imports must carry and validate the membership
   epoch or membership operation they depend on. Missing, stale, or mismatched
   authority dependencies must return a structured error.
3. Make product-shaped `PandaFactStore::open_sqlite` flows install authority
   snapshots from that source.
4. Add or promote a `PandaFactSyncScope::from_authority`-style constructor so
   sync scopes come from active authority members instead of caller-owned key
   maps.
5. Add an explicit process-role fact-policy bootstrap/fixture for Ployz
   fact-key grants. It is separate from p2panda-auth membership: membership
   answers "is this principal a writer/importer for the island?", while the
   policy fixture answers "which fact keys may that principal write/read?".
6. Keep manual `with_trusted_author_key`, `trust_author_key`, and
   `trust_replica_peer` only behind `#[cfg(test)]`, a harness-only feature, or a
   clearly named fixture type that product-shaped process paths cannot call. If
   a product-shaped path still needs one, stop the slice rather than documenting
   it as deferred.
7. Keep the Slice 035 policy: active writers/importers only; removed or
   demoted writers cannot import fresh facts without a future frontier proof.
8. Keep `FactAuthorizer` as the fact-key grant boundary. Membership proves a
   principal/key/role binding; it does not grant permission to write arbitrary
   fact keys.
9. Add a hard gate for fact-store rebuild semantics after membership changes:
   either persist accepted-at-ingest authority evidence/frontier data for
   already-accepted facts, or explicitly scope out restart-after-removal and
   restart-after-reinvite from this slice and test the conservative failure.

Acceptance:

- Fact import/write denial branches still return structured
  `UntrustedAuthorKey`, `AuthorKeyMismatch`, and
  `UnauthorizedReplicaImport`.
- A removed writer with the old key cannot import a fresh operation after
  authority rebuild.
- A reinvited principal with a new epoch/key can write only with the new
  binding.
- A principal with p2panda-auth writer membership but no matching Ployz
  fact-key write grant is denied with a structured grant error.
- If Unit 3 does not persist accepted-at-ingest authority evidence, restart
  after removal/reinvite must fail conservatively with a structured status
  instead of silently serving a misleading rebuilt projection.
- Writer-only principals cannot import as replicas; importer-only principals
  cannot author facts; a principal with both roles succeeds only on the
  appropriate side of the boundary.
- Cross-island membership and fact operations are rejected before changing
  authority snapshots, sync scopes, or projection output.

### Unit 4: Process-Serving Membership Path

Files:

- `MVP/e2e/src/process_role_harness.rs`
- `MVP/e2e/src/p2panda_net_process_serving_contract.rs`
- `MVP/e2e/src/p2panda_process_role_serving_contract.rs`

Plan:

1. Replace process role startup that receives repeated
   `--p2panda-trusted-author principal:key` flags with a membership store path
   or membership operation bootstrap path.
2. Include the non-net serving trust flags in the migration audit:
   `--p2panda-author`, `--p2panda-author-key`, and `TrustedP2pandaAuthor`.
   Keep publisher signing identity separate from serving trust bootstrap.
3. Ensure the serving projection role can restart, reopen membership, rebuild
   fact authority, then continue importing authorized fact operations.
4. Replace the overloaded `trusted_authors` process config with two explicit
   inputs:
   - root/membership bootstrap material, or a durable membership store path;
   - scoped fact-policy grants for the specific product proof.
5. Keep coordinator/local mutation socket absent from the remote update path.
   Serving roles should not need a daemon to know who may import already
   authorized replicated facts.
6. If a tiny bootstrap flag remains for the first root authority, name it as
   root bootstrap rather than general trusted-author plumbing.
7. Do not introduce a bespoke membership side channel. For this slice, choose
   one of two explicit transport shapes before implementation:
   - durable membership bootstrap file/store path for process roles, with live
     fact operations still using `PandaNetFactNode`;
   - a separate membership `LogSync` topic/store, named as membership
     transport, not hidden inside fact transport.
   Do not create a unified "any operation" envelope in this slice.

Acceptance:

- The process-serving p2panda-net E2E proves remote authorized route and DNS
  facts update gateway/DNS snapshots after membership-backed authority rebuild.
- An untrusted remote operation is rejected by membership-backed authority, not
  by a manually seeded trusted-author table.
- The process-serving p2panda-net E2E no longer accepts general
  trusted-author flags or calls product-path manual trust helpers.
- The E2E distinguishes membership failure from `UnauthorizedWrite` so fact-key
  grants cannot be confused with membership.

### Unit 5: E2E Membership Contract

Files:

- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/p2panda_auth_membership_contract.rs`
- `MVP/e2e/src/p2panda_net_fact_node_contract.rs`

Plan:

1. Add a product-shaped membership E2E that proves:
   - root creates island membership,
   - root adds writer,
   - root adds replica importer,
   - writer fact imports on another node,
   - demoted writer can no longer write but can import if granted replica
     importer access,
   - removed writer cannot write/import fresh facts,
   - reinvited principal with a new epoch/key can write,
   - old epoch/key cannot write or import after reinvite,
   - replaying an old add/key-binding operation cannot resurrect the old
     writer,
   - a valid island-A membership or fact operation offered to island B is
     rejected,
   - authority store restart preserves the same decisions, unless Unit 3's
     rebuild gate explicitly scoped out restart-after-removal/reinvite and
     replaced it with a conservative-failure proof.
2. Extend the p2panda-net fact-node contract only as far as needed to prove the
   Unit 4 deletion gate. If membership travels by bootstrap store path in this
   slice, the fact-node contract should prove facts use the rebuilt membership
   snapshot, not that membership operations themselves sync over fact transport.
3. Keep scale exactness: no approximate leakage. Unauthorized imports must be
   zero accepted.

Acceptance:

- `mvp-e2e -- all` includes the new membership contract.
- The contract's output names accepted/rejected counts and restart decisions.

### Unit 6: Deletion Ledger And Docs

Files:

- `MVP/slice-041-p2panda-auth-membership-substitution.md`
- `MVP/overall-plan.md`
- `MVP/architecture.md`
- `MVP/primitive-decisions.md`
- `MVP/e2e-proof-plan.md`
- `MVP/design-notes/p2panda-substitution-audit.md`
- `MVP/design-notes/semantic-leverage-loc.md`

Plan:

1. Add `Changed Since Last Slice` entries for the membership substitution.
2. Record whether `p2panda-auth` processor/store support was adopted now or
   deferred, with the exact reason.
3. Record every deleted manual trust path and every retained fixture path.
4. Include the containment grep grouped by file path. Every remaining
   `trust_author_key`, `trust_replica_peer`, `trusted_author_keys`,
   `trusted_replica_peers`, `p2panda-trusted-author`, `p2panda-author`,
   `p2panda-author-key`, and `TrustedP2pandaAuthor` match must be marked
   fixture-only or publisher-signing-only, or the slice is not complete.
5. Update the semantic leverage ledger with deleted LOC and reduced product
   wiring.
6. Keep the no-RC-iroh constraint explicit if any transport dependency changes.

## Review And Simplify Cadence

Do not run a heavy review workflow for tiny mechanical fixes. For this slice:

1. Commit the processor/API fit check separately if it changes code.
2. Commit durable membership store work separately.
3. Commit product path wiring separately.
4. Commit E2E/docs separately.
5. Run a simplify pass after Unit 2 and again after Unit 4, focused on deleting
   duplicate trust plumbing rather than polishing fixtures.
6. Run review subagents before the final slice commit because this slice
   changes authorization boundaries.

Review focus:

- correctness: stale removed writers, author-key mismatch, wrong island,
  duplicate operations, restart rebuild;
- security: manager-only membership mutation, root anchoring, signature
  payload coverage, no subject-grant confusion;
- maintainability: no second custom membership CRDT hidden under p2panda-auth;
- testing: product-shaped process paths no longer succeed because manual trust
  was preloaded.

## Verification

Target commands:

```text
cd MVP && cargo check --workspace
cd MVP && cargo test -p mvp-p2panda-authz
cd MVP && cargo test -p mvp-p2panda-facts
cd MVP && cargo test -p mvp-p2panda-transport
cd MVP && cargo run -p mvp-e2e -- p2panda-auth-membership-contract
cd MVP && cargo run -p mvp-e2e -- p2panda-net-fact-node-contract
cd MVP && cargo run -p mvp-e2e -- p2panda-net-process-serving-contract
cd MVP && cargo run -p mvp-e2e -- all
git diff --check
```

Deletion/containment gates:

```text
cd MVP && rg "p2panda-trusted-author|p2panda-author|p2panda-author-key|TrustedP2pandaAuthor|trusted_author_keys|trusted_replica_peers|trust_replica_peer|trust_author_key" \
  p2panda-authz p2panda-facts p2panda-transport e2e/src
```

The grep does not have to be empty if fixture APIs remain, but every remaining
match must be classified in the slice report as one of:

- deleted in this slice;
- retained fixture only.

`deferred` is not an acceptable classification for product-shaped paths in this
slice. If a product path still needs manual trust, split the slice and do not
mark Slice 041 complete.

## Risks

- The `p2panda-auth` processor may not fit the current custom Ployz
  key-binding envelope without leaking p2panda generic types everywhere. If so,
  use p2panda-auth's group CRDT directly for this slice and document the
  processor as a later internal-storage simplification.
- Removing manual trust too aggressively could break deterministic E2E setup.
  Prefer a named fixture API over product-shaped CLI flags.
- Latest-state authorization is intentionally conservative. This may reject
  old legitimate operations authored before removal. That is acceptable until a
  future frontier/cutoff proof exists; accepting stale-key fresh operations is
  worse.
- Membership convergence is not command consistency. Do not introduce quorum or
  active-partition checks while deleting trust maps.
