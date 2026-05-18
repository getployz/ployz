---
title: Slice 042 Membership-backed ACME and Sync Plan
status: active
created: 2026-05-19
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/semantic-leverage-loc.md
  - MVP/slice-041-p2panda-auth-membership-substitution-plan.md
external:
  - https://docs.rs/p2panda-auth
  - https://docs.rs/p2panda-net
  - https://docs.rs/p2panda-sync/latest/p2panda_sync/log_sync/
---

# Slice 042 Membership-backed ACME and Sync Plan

## Problem Frame

Slice 041 made durable p2panda-auth membership the normal authority source for
process-serving fact stores. That closes the most visible process-role trust
hole, but two high-value proofs still exercise the old manual shape:

- `p2panda-acme-http01-contract` builds `trusted_authors`, calls
  `with_trusted_author_key`, calls `trust_replica_peer`, and builds sync scope
  from caller-owned key maps.
- `p2panda-sync-fact-source-contract` still proves sync primarily through
  manual trusted-author and replica-peer setup.

Those contracts are close to product behavior. Leaving them on manual trust
would make the membership substitution look better than it is: ACME would still
be proving the old authority model, and future product slices would have two
plausible ways to answer "who may write/import this island?"

This slice moves ACME and the core p2panda sync proof onto the durable
membership authority path. It should not invent new transport, command, or
serving concepts. The work is a deletion/containment slice: product-shaped
proofs use membership snapshots; manual trust APIs either become clearly named
fixtures or get a deletion trigger.

## Dependency Scout

Checked on 2026-05-19:

- `p2panda-auth` already provides eventually consistent group state,
  `Pull`/`Read`/`Write`/`Manage` access levels, strict manager-only membership
  changes, and strong-removal conflict handling. Slice 041 already wraps this
  as `IslandAuthoritySnapshot`; Slice 042 should reuse that seam rather than
  reaching deeper into p2panda-auth.
- `p2panda-net` is still the maintained transport carrier for topics and
  `LogSync`; it is not the scope of this slice except insofar as sync/import
  authority must be independent from transport delivery.
- `p2panda-sync` documents log-sync as generic over numbered linked logs. The
  current MVP already wraps this in `sync_panda_fact_stores` and
  `PandaFactSyncScope`; the next simplification is feeding that scope from
  membership with `PandaFactSyncScope::from_authority`, not replacing sync.

## Scope

In scope:

- Convert the p2panda ACME HTTP-01 E2E proof to open stores from
  `PandaFactAuthoritySource` and build sync scope from an
  `IslandAuthoritySnapshot`.
- Convert the p2panda sync fact-source E2E proof to use membership-backed
  writers and replica importers for its main product-shaped path.
- Keep tests proving unauthorized replica import, wrong-island sync, missing
  scope author, duplicate/no-op sync, conflict candidate preservation, payload
  read grants, and projection rebuild.
- Reuse the shared p2panda membership fixture instead of adding more local
  membership setup to E2E files.
- Audit manual trust call sites only in the ACME contract, the main sync
  contract path, and the p2panda-net fact-node regression surface touched by
  this slice. Other product-shaped call sites are recorded as follow-up
  migration targets, not silently swept into this slice.
- Update the semantic-leverage LOC note with the current "what got simpler vs
  what grew" read.

Out of scope:

- No p2panda-net membership-operation replication. Durable membership bootstrap
  plus authority snapshots are enough for this slice.
- No direct iroh or p2panda-net API reshaping.
- No production ACME issuer integration beyond the HTTP-01 fact/serving canary.
- No quorum, witness acks, or active-partition membership checks.
- No full removal of manual trust APIs if low-level tests still need them. The
  bar is that product-shaped E2Es stop using them and remaining uses are named
  fixtures.
- No root workspace or existing `crates/` edits.

## Non-Negotiable Landing Gates

- `p2panda-acme-http01-contract` must not call
  `PandaSqliteOpenConfig::with_trusted_author_key`,
  `PandaFactStore::trust_replica_peer`, `SharedPandaFactStore::trust_replica_peer`,
  or `PandaFactSyncScope::from_trusted_authors`.
- The main `p2panda-sync-fact-source-contract` path must use membership-backed
  writer and replica-importer authority. Any remaining manual-trust subcase
  must be explicitly named as a low-level fallback/fixture probe.
- Replica import remains a separate authority from writing. Writers do not
  imply replica importers; replica importers do not imply writers.
- Fact-key grants remain Ployz-owned. Membership may authorize the principal as
  an island writer, but the bus/session grant must still authorize the key
  pattern.
- No new public authority mode. Use the existing
  `PandaFactAuthoritySource`/`IslandAuthoritySnapshot` seam.
- The slice must reduce or contain duplicate setup. Do not copy another local
  membership fixture into ACME or sync.

## Implementation Units

### Unit 1: ACME Uses Membership Authority

Files:

- `MVP/e2e/src/p2panda_acme_http01_contract.rs`
- `MVP/e2e/src/p2panda_projection_fixture.rs`
- `MVP/p2panda-facts/src/lib.rs`

Plan:

1. Characterize the existing ACME contract metrics before changing authority:
   successful takeover, stale mutation rejection, scoped grant rejection,
   replica-required sync failure, duplicate sync no-op, SQLite rebuild, clear
   removes HTTP-01 serving.
2. Instantiate the existing shared p2panda membership fixture for issuer A,
   issuer B, DNS writer, left replica, and right replica. Extend that fixture
   only if ACME needs data it cannot already expose.
3. Open left/right p2panda stores with `with_authority_source` from that
   fixture instead of `with_trusted_author_key`.
4. Build the sync scope with `PandaFactSyncScope::from_authority`.
5. Remove direct replica peer trust calls; replica sessions should pass because
   the membership snapshot marks them as replica importers.
6. Keep the "trusted replica required" assertion by using an untrusted replica
   session and asserting the structured sync error still surfaces.

Test Scenarios:

- ACME issuer A publishes HTTP-01, issuer B takeover is deterministic after
  expiry, and projection serves only the selected challenge.
- A bus principal with fact-key scope for a different challenge is still
  rejected even though the author is a membership writer.
- Sync with an untrusted replica principal fails before import.
- Reopened right-side store rebuilds projection from membership-backed
  authority and serving preserves last-good behavior.

Verification:

```text
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-acme-http01-contract
rg -n "with_trusted_author_key|trust_replica_peer|from_trusted_authors|trusted_authors" MVP/e2e/src/p2panda_acme_http01_contract.rs
```

### Unit 2: Sync Contract Uses Membership Authority

Files:

- `MVP/e2e/src/p2panda_sync_fact_source_contract.rs`
- `MVP/e2e/src/p2panda_projection_fixture.rs`
- `MVP/p2panda-facts/src/lib.rs`

Plan:

1. Move the main persistent-store sync path to a membership fixture containing
   all writers and both replica importers.
2. Open both stores with `PandaFactAuthoritySource`.
3. Replace caller-owned sync scopes with `PandaFactSyncScope::from_authority`.
4. Keep or add negative coverage for:
   - replica island mismatch,
   - untrusted replica principal,
   - scope author missing from authority,
   - received operation whose author is not an active writer.
5. Any remaining manual-trust subcase must be renamed or commented as a
   fallback compatibility fixture, not the product-shaped sync path.

Test Scenarios:

- First sync imports missing operations and conflicts; repeat sync is a no-op.
- 10,000-operation sync still preserves exact candidate counts and no
  cross-island leakage.
- Unauthorized replica and missing scope-author checks return structured
  sync errors without importing facts.

Verification:

```text
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-sync-fact-source-contract
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts
```

### Unit 3: Targeted Manual Trust Containment

Files:

- `MVP/p2panda-facts/src/lib.rs`
- `MVP/e2e/src/p2panda_net_fact_node_contract.rs`
- `MVP/e2e/src/p2panda_projection_fixture.rs`
- `MVP/primitive-decisions.md`
- `MVP/slice-042-membership-backed-acme-sync.md`

Plan:

1. Audit the ACME contract, the main sync product path, and the p2panda-net
   fact-node regression surface for:
   - `PandaTrustedAuthorKey`,
   - `with_trusted_author_key`,
   - `trust_replica_peer`,
   - `PandaFactSyncScope::from_trusted_authors`.
2. Delete product-shaped uses in those targeted paths.
3. For low-level tests in those files that intentionally exercise manual fallback, make that
   visible in names or comments. The reader should know it is not the product
   path.
4. Record remaining manual-trust call sites outside this slice as follow-up
   targets instead of editing unrelated product proofs.
5. If public fallback APIs cannot move behind a test/harness feature without
   excessive churn, document the exact deletion trigger and keep the API small.
6. Do not add a broad `AuthorityConfig` enum or compatibility shim. Product
   callers should pass an authority source.

Test Scenarios:

- `rg` shows no manual trust calls in ACME or the main sync product path.
- Manual trust tests in the targeted files remain only as clearly named
  fallback fixtures.
- Existing p2panda-net fact-node proofs still reject untrusted authors and
  unauthorized replica import.

Verification:

```text
rg -n "PandaTrustedAuthorKey|with_trusted_author_key|trust_replica_peer|from_trusted_authors" MVP/e2e/src/p2panda_acme_http01_contract.rs MVP/e2e/src/p2panda_sync_fact_source_contract.rs MVP/e2e/src/p2panda_net_fact_node_contract.rs
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-contract
```

### Unit 4: Semantic Leverage Ledger

Files:

- `MVP/design-notes/semantic-leverage-loc.md`
- `MVP/overall-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/slice-042-membership-backed-acme-sync.md`

Plan:

1. Add a slice report with a small LOC ledger:
   feature/business LOC, shared primitive LOC, adapter/backend LOC,
   E2E/harness LOC, old equivalent LOC made irrelevant, and repeated glue
   deleted.
2. Record the current LOC signal honestly:
   deploy and machine removal are real wins; serving/ACME are not yet raw LOC
   wins; `p2panda-facts` and `process_role_harness` are accretion risks.
3. Update the decision ledger with what manual trust changed into.
4. Update the overall plan's next-step pointer after the slice lands.

Verification:

```text
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
git diff --stat
```

## Final Verification

Run before committing the completed slice:

```text
cargo fmt --manifest-path MVP/Cargo.toml --all
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts
cargo test --manifest-path MVP/Cargo.toml -p mvp-e2e --all-targets
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-acme-http01-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-sync-fact-source-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
git diff --check
```

The PR must remain draft.
