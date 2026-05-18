---
title: Slice 034 p2panda Auth And Substrate Simplification Plan
status: active
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution.md
  - MVP/design-notes/p2panda-substitution-audit.md
  - MVP/slice-032-p2panda-net-crates-io-substitution.md
  - MVP/slice-033-environment-branch-promote-rollback.md
external:
  - https://docs.rs/p2panda-net/latest/p2panda_net/
  - https://docs.rs/p2panda-auth/latest/p2panda_auth/
  - https://docs.rs/p2panda-store/latest/p2panda_store/
  - https://docs.rs/p2panda-sync/latest/p2panda_sync/
  - https://docs.rs/p2panda-core/latest/p2panda_core/
  - https://docs.rs/p2panda-blobs/latest/p2panda_blobs/
---

# Slice 034 p2panda Auth And Substrate Simplification Plan

## Problem Frame

The MVP has already crossed the important p2panda boundary:

- durable facts are p2panda operations behind `FactSource`,
- persistent p2panda SQLite stores rebuild Ployz indexes,
- p2panda-sync replicates store contents,
- p2panda-net carries fact operations over the maintained iroh/gossip/log-sync
  stack,
- crates.io p2panda `0.5.2` is now used for the transport path, so the earlier
  "can we use net?" blocker is gone.

The remaining question is sharper: what custom substrate can we still delete or
avoid before hardening more product commands?

This slice is a deep substitution investigation with small compile-backed
prototypes. Its job is to prove where p2panda can own more plumbing and where
Ployz must keep product semantics. It should not add a new product primitive.

The bias is toward adoption. The comparison is not p2panda versus a perfect
Ployz implementation; it is maintained p2panda crates versus MVP code that is
still young and likely more expensive to maintain.

## Requirements Trace

- `VISION.md`: the cluster should stay small and command-shaped; generic
  substrate should not bury product rules in custom machinery.
- `MVP/overall-plan.md`: before each slice, scout crates and lean on
  well-tested substrate plumbing where it fits.
- `MVP/architecture.md`: p2panda is the preferred signed fact substrate, but
  Ployz still owns subject bus semantics, command conflicts, projections, and
  gateway/DNS behavior.
- `MVP/e2e-proof-plan.md`: the final MVP must be proven by E2E tests and
  semantic-leverage evidence, not by architectural intent.
- Prior p2panda audits: `p2panda-auth` was identified as the next serious
  candidate for island membership and revocation after p2panda persistence and
  transport were proven.

## Current Evidence

Checked on 2026-05-18:

- `p2panda-net 0.5.2` is on crates.io and is already used by
  `mvp-p2panda-transport`. It provides address book, iroh endpoint, discovery,
  gossip, log sync, mDNS, and supervisor modules behind feature flags.
- `p2panda-net` depends on the non-RC iroh `0.96` family. The MVP already
  aligned `mvp-iroh` to that line in Slice 032.
- `p2panda-core 0.5.2` owns signed append-only operations, Ed25519 authorship,
  BLAKE3 body hashes, extensible headers, fork tolerance, and partial sync
  metadata.
- `p2panda-store 0.5.2` owns Memory and SQLite operation/log stores and atomic
  transaction traits, but explicitly does not validate log integrity or solve
  all application-layer indexes.
- `p2panda-sync 0.5.2` owns data-type-agnostic two-party sync protocols over
  `Sink` / `Stream` pairs and is already used by `sync_panda_fact_stores`.
- `p2panda-auth 0.5.2` owns eventually consistent group membership, Pull/Read/
  Write/Manage access levels, strict manager-only group modification, and a
  strong-removal concurrency resolver.
- `p2panda-blobs 0.5.2` has implementation files in the crate source, but the
  published crate root currently exports no usable API and documents 0%.
  Treat it as not adoptable in this slice.

Current custom substrate pressure:

| Area | File | Current LOC | Investigation target |
| --- | ---: | ---: | --- |
| p2panda fact store adapter | `MVP/p2panda-facts/src/lib.rs` | 3287 | Keep operation/store adapter, but try to replace custom trusted-author/replica membership with p2panda-auth. |
| p2panda transport wrapper | `MVP/p2panda-transport/src/*.rs` | 2149 | Keep for now; it already wraps p2panda-net and hides raw types from product crates. |
| iroh docs fact wrapper | `MVP/iroh/src/facts.rs` | 1689 | Retire as a product proof path if p2panda-backed scenarios cover its remaining semantics. |
| process JSON fact source | `MVP/e2e/src/process_fact_source.rs` | 682 | Replace process-serving proofs with persistent p2panda store variants where possible. |
| in-memory bus fact source | `MVP/bus/src/facts.rs`, `MVP/projection/src/bus_source.rs` | 857 | Keep as fixture for bus/projection unit tests; stop treating it as a product-shaped fact substrate. |

Subagent inventory adds two smaller but important targets:

- `PandaFactWireEnvelope` is a custom `PFO1` header/body frame around p2panda
  operations. It may be replaceable with p2panda raw-operation encoding or a
  narrower transport payload type.
- `MVP/p2panda-transport/src/quarantine_log.rs` manually creates signed
  p2panda wrapper operations and a topic map so p2panda-net can carry fact
  envelopes. It might be shrinkable by using more of p2panda-net/log-sync's
  existing store flow directly.

## Scope

In scope:

- Build a small `p2panda-auth` compile-backed spike inside `MVP/` that maps
  Ployz island membership to p2panda group state.
- Inspect the p2panda operation encoding/log-sync path for a possible
  `PandaFactWireEnvelope` or quarantine-log simplification.
- Decide whether `p2panda-auth` can replace:
  - `PandaFactStore::trusted_author_keys`,
  - `PandaFactStore::trusted_replica_peers`,
  - manual sync-scope trusted-author construction,
  - at least part of mesh membership/tombstone validation.
- Identify the smallest production-shaped adoption slice if the spike works.
- Identify any old proof paths that can be deleted or downgraded to fixtures
  after the adoption slice.
- Do not delete or downgrade existing fixture/proof paths in this slice. This
  slice nominates deletion candidates and names their proof gates; deletion
  happens in the next implementation slice unless Unit 5 finds a tiny
  behavior-preserving encoding simplification.
- Update decision/proof docs with an honest substitution scorecard.

Out of scope:

- No product command changes.
- No migration outside `MVP/`.
- No replacement of PloyzBus subject/request/queue/bridge grants.
- No replacement of deterministic projection reducers.
- No p2panda-blobs adoption unless the published crate root exposes a usable
  API during implementation.
- No generic workflow/`mvp-commands` primitive.
- No quorum, consensus, or active-partition membership semantics.

## Design Decisions To Test

### 1. p2panda-auth May Own Island Membership, Not Bus Permissions

The most promising substitution is a new MVP-local island membership adapter:

```text
p2panda-auth group state
  -> member has Manage/Write/Read/Pull
  -> Ployz maps member keys to principals and fact access
  -> PandaFactStore checks this before accepting writes/imports/sync
```

Do not ask p2panda-auth to express:

- wildcard subjects,
- queue permissions,
- temporary reply permissions,
- bridge imports/exports,
- command-specific preconditions.

Those are PloyzBus and command semantics.

### 2. Author-Key Trust Should Become Membership Evidence

Today `PandaFactStore` has an in-memory
`BTreeMap<(IslandId, PrincipalId), PublicKey>` and callers manually seed it
through `trust_author_key`. That is acceptable for the early MVP, but it is
exactly the kind of custom substrate that will accrete.

The spike should test whether p2panda-auth group operations can be the durable
source of:

- principal membership,
- principal p2panda public key,
- member access level,
- removal/demotion domination under concurrency,
- re-add with a new key/epoch.

If p2panda-auth does not carry public-key binding directly, use a small Ployz
membership fact payload associated with the group operation. Do not hide this
in an in-memory map.

### 3. Replica Import Is A Non-Writer Membership Role

Today `trusted_replica_peers` is another local set. The spike should model
replica import permission as an active group member with a non-write access
level plus a typed `IslandMemberCondition::ReplicaImporter` p2panda-auth
condition. Do not give import-only replicas `Write` access: p2panda-auth's
`Access::is_write()` checks the access level, not the Ployz condition, and the
MVP currently keeps replica import permission separate from operation-author
write permission.

The adoption target is:

```text
import_replica_operation(session, operation)
  -> session principal is active in island membership
  -> principal has Pull/Read + IslandMemberCondition::ReplicaImporter
  -> principal does not satisfy can_write_member
  -> operation author is active and has Write for the fact key
  -> original Ployz fact grant still allows the specific key
```

The last line remains Ployz-owned. p2panda-auth tells us who is an island
member and with what broad data access; Ployz grants still decide whether that
member may write `/facts/deploy/>` or `/facts/node/>`.

### 4. Strong Removal Must Be Proven Against Machine Remove

p2panda-auth's default resolver uses strong removal. The spike must exercise
the exact cases that matter for Ployz:

- manager removes a node while the removed node concurrently writes a fact;
- two managers concurrently remove/demote each other;
- a removed node is re-added with a new epoch/key;
- transitive operations from a removed member are rejected or projected as
  unauthorized after membership reduction.

If this maps cleanly, the next product slice should use it to simplify machine
membership/import authority. If it does not, keep p2panda-auth out and record
the mismatch precisely.

### 5. Do Not Delete Fixtures Until A Product Proof Replaces Them

`BusFactSource`, `ProcessFactSource`, and `IrohDocsFactSource` are not all the
same kind of debt.

- `BusFactSource` is still useful for small deterministic unit tests and early
  bus/projection tests.
- `ProcessFactSource` is mostly obsolete once process-role serving contracts
  use persistent p2panda stores.
- `IrohDocsFactSource` is parked substrate. It should not receive more product
  behavior, and its remaining E2Es should either be marked historical or ported
  to p2panda-backed proofs.

This slice should produce a deletion/readiness map. It should not delete
fixtures or retire E2E scenarios; that is the follow-up slice once the map names
the replacement proof gate.

## Implementation Units

### Unit 1: p2panda-auth API Spike

Files:

- `MVP/p2panda-authz/Cargo.toml`
- `MVP/p2panda-authz/src/lib.rs`
- `MVP/Cargo.toml`
- `MVP/slice-034-p2panda-auth-and-substrate-simplification.md`

Plan:

1. Add a small experimental crate under `MVP/p2panda-authz`, package name
   `mvp-p2panda-authz`. Keep this name for the slice so file paths and
   verification commands stay unambiguous.
2. Depend on `p2panda-auth = { version = "0.5.2", features = ["serde"] }`.
3. Model p2panda-auth handles with stable `Copy` values, not borrowed Ployz
   string newtypes. Derive handles from stable hashes or public keys, then keep
   an explicit read-model binding back to `IslandId` and `PrincipalId`:
   - `IslandGroupId`,
   - `IslandMemberId`,
   - `IslandOperationId`,
   - `IslandMemberEpoch`,
   - `IslandMemberKey`.
4. Add the minimal in-memory p2panda-auth operation and orderer types required
   by the `Groups` API. The spike should make ordering explicit enough to test
   concurrent remove/write and manager-removal cases; it should not hide these
   API-shaping choices in test helpers.
5. Model the only p2panda-auth condition in this spike as
   `IslandMemberCondition::ReplicaImporter`. Public-key binding is represented
   by a Ployz-owned `IslandMemberKeyBinding` read model in this crate, not by a
   durable Ployz fact schema yet. The slice report must say whether that binding
   should become a fact in the adoption slice.
6. Build a minimal adapter that can:
   - create a group with a root/admin manager,
   - add a node/member with Write access,
   - add a replica/import member with Pull or Read access plus
     `IslandMemberCondition::ReplicaImporter`,
   - remove/demote a member,
   - reduce concurrent operations using p2panda-auth's default resolver,
   - answer `can_write_member`, `can_import_replica`, and `is_active_member`.
7. Keep p2panda-auth generic types inside this crate. Product/domain crates
   should see Ployz-owned membership read models only.

Test scenarios:

- root creates island group and is manager,
- manager adds node member with Write access,
- non-manager cannot add or remove members,
- removed writer no longer satisfies `can_write_member`,
- removed replica no longer satisfies `can_import_replica`,
- replica importer never satisfies `can_write_member`,
- re-add with new epoch/key is active while old-key operations are not,
- concurrent remove versus write resolves in favor of strong removal,
- concurrent manager removal/demotion behaves as documented by p2panda-auth and
  is recorded in the report even if it is not the final Ployz policy.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-authz --all-targets`
- `cargo clippy --manifest-path MVP/Cargo.toml -p mvp-p2panda-authz --all-targets -- -D warnings`

### Unit 2: Fact-Store Membership Substitution Design

Files:

- `MVP/p2panda-facts/src/lib.rs`
- `MVP/design-notes/p2panda-substitution-audit.md`
- `MVP/primitive-decisions.md`
- `MVP/slice-034-p2panda-auth-and-substrate-simplification.md`

Plan:

1. Do not rewrite `PandaFactStore` in this unit unless Unit 1 proves the API is
   straightforward and the change remains small.
2. Produce a concrete adapter design for replacing:
   - `trusted_author_keys`,
   - `trusted_replica_peers`,
   - `PandaFactSyncScope::trusted_authors`,
   - manual reopen-time trusted-author seeding.
3. Identify the minimum code path for a follow-up adoption slice:
   - likely `PandaFactStore` receives an `IslandMembershipView` trait,
   - p2panda-auth-backed implementation supplies active member/key/access,
   - existing tests keep an in-memory fixture implementation.
4. If implementation is small enough, add the trait and a no-op/in-memory
   adapter without changing behavior. Otherwise keep this unit as a design
   report and avoid speculative abstractions.

Test scenarios if code changes:

- existing `mvp-p2panda-facts` tests still pass unchanged,
- removed/untrusted author remains rejected,
- unauthorized writer remains projected as unauthorized,
- trusted replica import remains explicit.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts --all-targets`
- `cargo clippy --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts --all-targets -- -D warnings`

### Unit 3: Fixture Deletion Readiness Audit

Files:

- `MVP/e2e/src/process_fact_source.rs`
- `MVP/projection/src/bus_source.rs`
- `MVP/bus/src/facts.rs`
- `MVP/iroh/src/facts.rs`
- `MVP/e2e/src/*`
- `MVP/e2e-proof-plan.md`
- `MVP/design-notes/p2panda-substitution-audit.md`

Plan:

1. Inventory every remaining use of:
   - `ProcessFactSource`,
   - `BusFactSource`,
   - `IrohDocsFactSource`,
   - direct `BusActorHandle::write_fact_payload` product-shaped writes.
2. Classify each use:
   - keep as unit fixture,
   - replace with `SharedPandaFactStore` now,
   - replace after p2panda-auth membership adoption,
   - keep as historical iroh-docs compatibility proof.
3. Find at least one concrete deletion or downgrade candidate for the next
   slice. The likely candidates are:
   - process-role JSON fact-source paths now duplicated by p2panda process
     serving contracts,
   - old docs-backed ACME/iroh-docs product canaries now superseded by
     p2panda ACME and p2panda-net process serving.
4. Do not delete the fixtures in this investigation slice unless the
   replacement proof already exists and the diff is obviously small.

Deliverable:

- A table in the slice report listing each fixture, current callers, verdict,
  and exact next deletion test gate.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-e2e --all-targets`
- No full E2E required for a docs-only classification, but any deletion must
  run `MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all`.

### Unit 4: Blob Substitution Decision

Files:

- `MVP/design-notes/p2panda-substitution-audit.md`
- `MVP/primitive-decisions.md`
- `MVP/slice-034-p2panda-auth-and-substrate-simplification.md`

Plan:

1. Re-check `p2panda-blobs 0.5.2` during implementation.
2. If the crate root still exports no usable API, record "defer" explicitly.
3. If a newer compatible release exists, compile a tiny throwaway spike before
   proposing adoption.
4. Do not add blob migration to this slice. The only outcome is a go/no-go note
   and a candidate future slice if the API is real.

Verification:

- `cargo info p2panda-blobs --verbose`
- `cargo check` for any retained compile spike, if one is added.

### Unit 5: Wire Envelope And Quarantine-Log Simplification Scout

Files:

- `MVP/p2panda-facts/src/lib.rs`
- `MVP/p2panda-transport/src/quarantine_log.rs`
- `MVP/p2panda-transport/src/node.rs`
- `MVP/p2panda-transport/src/fact_node.rs`
- `MVP/p2panda-transport/src/tests.rs`
- `MVP/design-notes/p2panda-substitution-audit.md`

Plan:

1. Inspect p2panda-core/store/sync APIs for an existing raw-operation encoding
   or operation payload shape that can replace `PandaFactWireEnvelope`.
2. Inspect `p2panda-net::LogSync` and `SyncHandle::publish` usage to determine
   whether `PandaNetQuarantineLog` can stop manually constructing signed
   wrapper operations.
3. If either replacement is tiny and obviously behavior-preserving, implement
   it as a simplification commit.
4. If replacement would disturb the fact-node E2E contracts, record exact
   blockers and leave the wrapper alone until the p2panda-auth/membership
   answer is known.

Test scenarios if code changes:

- p2panda wire envelope round-trips or the replacement encoding round-trips,
- malformed transport payload remains rejected,
- stream refresh replay suppression still suppresses only wrapper replay,
- fact-node duplicate/conflict/deferred/rejected outcomes remain structured.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts --all-targets`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport --all-targets --features harness`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-contract`

### Unit 6: Decision Ledger And Next Slice Recommendation

Files:

- `MVP/overall-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/e2e-proof-plan.md`
- `MVP/design-notes/p2panda-substitution-audit.md`
- `MVP/slice-034-p2panda-auth-and-substrate-simplification-plan.md`
- `MVP/slice-034-p2panda-auth-and-substrate-simplification.md`

Plan:

Write the final report with a clear scorecard:

| Candidate | Adopt now | Adopt later | Reject/defer | Why |
| --- | --- | --- | --- | --- |
| p2panda-auth for island membership | TBD | TBD | TBD | Must be backed by Unit 1 tests. |
| p2panda-auth for bus grants | No | Maybe never | Likely reject | Ployz needs NATS-shaped subject/queue/reply/bridge semantics. |
| p2panda-blobs | TBD | TBD | TBD | Depends on crate-root API. |
| p2panda raw operation encoding for fact wire | TBD | TBD | TBD | Depends on Unit 5 API scout. |
| p2panda-net/store path for quarantine log | TBD | TBD | TBD | Depends on Unit 5 API scout. |
| Delete ProcessFactSource | TBD | TBD | TBD | Depends on current E2E caller map. |
| Retire IrohDocsFactSource product proofs | TBD | TBD | TBD | Depends on coverage overlap with p2panda proofs. |

The report must name the next implementation slice. Good possible outcomes:

- `Slice 035: p2panda-auth-backed island membership for fact import`, if Unit 1
  proves the fit.
- `Slice 035: delete obsolete process/iroh fact fixtures`, if auth is not ready
  but deletion coverage is.
- `Slice 035: product command proof`, if no generic substrate substitution is
  currently worth doing.

Verification:

- `git diff --check -- MVP/slice-034-p2panda-auth-and-substrate-simplification-plan.md MVP/slice-034-p2panda-auth-and-substrate-simplification.md MVP/overall-plan.md MVP/primitive-decisions.md MVP/e2e-proof-plan.md MVP/design-notes/p2panda-substitution-audit.md`
- Run code tests listed in Units 1-5 if code was added.

## Review Focus

Use subagent review on the completed slice because this investigation affects
architecture direction, even if the code diff is small.

Ask reviewers to focus on:

- whether p2panda-auth is being overfit to Ployz semantics,
- whether any proposed deletion loses an E2E proof,
- whether the plan keeps PloyzBus permissions separate from membership,
- whether the proposed next slice is a real maintenance win or just another
  abstraction.

## Done Criteria

This slice is complete when:

- p2panda-auth has a compile-backed fit/no-fit answer for island membership and
  strong removal;
- the remaining custom substrate has a ranked replacement/deletion map;
- p2panda-blobs has a current adopt/defer decision;
- docs record the decision and exact next slice;
- all affected tests and clippy checks pass.
