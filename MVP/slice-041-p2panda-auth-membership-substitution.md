---
title: Slice 041 p2panda-auth Membership Substitution
status: completed
created: 2026-05-19
origin:
  - MVP/slice-041-p2panda-auth-membership-substitution-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution-audit.md
---

# Slice 041 p2panda-auth Membership Substitution

## Unit 1 Result: Processor Fit Check

`p2panda-auth`'s `GroupsProcessor` is compile-backed in
`mvp-p2panda-authz` now. The check proves it can process group create/add
operations into a persistent p2panda-store group state with Ployz's
`ReplicaImporter` condition.

Disposition for this slice: do not adopt `GroupsProcessor` as the authority
store yet.

Reason:

- `GroupsProcessor` fixes group/member identity to p2panda `VerifyingKey` and
  operation identity to p2panda `Hash`.
- Ployz still needs durable `(island, principal, epoch, author key)` bindings,
  canonical root anchoring, signer/member binding checks, and explicit
  introduced-binding validation around p2panda-auth group operations.
- Adopting the processor directly would either leak p2panda generic identities
  into the fact-store authority boundary or require a second mapping layer that
  does not delete manual trust yet.

Unit 2 should persist signed membership operations as validated p2panda
operations, but replay them through the current Ployz-owned
`GroupCrdt<AuthId, IslandOperationId, ...>` wrapper. Revisit
`GroupsProcessor` later as a storage/ordering optimization only if it deletes
code without weakening Ployz root and principal/key/epoch semantics.

Verification:

```text
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-authz groups_processor_fit_check_uses_verifying_key_hash_identity_model
```

## Unit 2 Result: Durable Membership Store

`mvp-p2panda-authz` now has `IslandAuthzStore`, a SQLite-backed durable store
for signed island membership operations. It stores validated p2panda operations
with `IslandMembershipExtensions`, then replays those operations through the
existing Ployz-owned `GroupCrdt<AuthId, IslandOperationId, ...>` wrapper chosen
in Unit 1.

The store deliberately does not adopt `GroupsProcessor` yet. The p2panda
operation envelope is used for durable signed storage and log integrity; Ployz
still owns root anchoring, principal/key/epoch bindings, introduced-binding
checks, and the exact authority snapshot shape consumed by fact stores.

Proofs added:

- SQLite store reopens and reconstructs root plus writer authority.
- A second root create is rejected with `RootAlreadyPinned`.
- Opening an existing store with the wrong root authority fails closed before
  returning the store.
- The previous in-memory membership log now shares the same p2panda operation
  builder as the SQLite store.

Verification:

```text
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-authz
```

## Unit 3 Result: Durable Fact Authority Source

`mvp-p2panda-facts` now has `PandaFactAuthoritySource`, a narrow bridge from
durable p2panda-auth membership state into fact-store authority snapshots.
`PandaSqliteOpenConfig::with_authority_source` installs those snapshots during
fact-store open, so callers no longer need to manually translate a durable
membership store into trusted author keys.

The product-shaped process-serving paths now consume durable membership
authority instead of general trusted-author flags:

- `serving-projection --fact-source p2panda-sqlite` takes a membership store
  path plus root authority identity.
- `p2panda-net-serving-projection` takes the same membership authority path
  plus explicit `--p2panda-fact-writer` principals for local fact-key grant
  policy.
- Replica import authority comes from membership-backed
  `ReplicaImporter(Pull)` membership, not `trust_replica_peer`.
- The p2panda-net receiver validates the configured replica principal at
  startup and refreshes the installed membership authority before import
  batches, so a long-running process does not keep accepting fresh operations
  against a stale writer snapshot.
- Fact-key authorization remains Ployz-owned: the process harness still grants
  `/facts/>` write/read policy explicitly to requested writers after verifying
  each one is active in membership.

Accepted-at-ingest evidence remains deliberately strict: reopened/rebuilt
authority-backed fact stores validate stored operations against the current
membership snapshot and fail closed for removed/demoted writers. That keeps the
Slice 035 gate intact until a future fact-log frontier proof makes historical
pre-removal imports safe.

Verification:

```text
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts sqlite_open_config_installs_durable_membership_authority_source
cargo check -p mvp-e2e
cargo run -p mvp-e2e -- p2panda-process-role-serving-contract
cargo run -p mvp-e2e -- environment-branch-promote-rollback-contract
cargo run -p mvp-e2e -- p2panda-net-process-serving-contract
cargo test -p mvp-e2e
```

## Unit 4 Result: Process Role Membership Wiring

Product-shaped process roles now receive p2panda fact authority from durable
membership, not general trusted-author command-line flags. The serving
projection role and p2panda-net serving projection role both open the
membership store, validate configured fact writers against active membership,
install membership authority into the fact store, and keep local fact-key grant
policy explicit. The p2panda-net receiver also validates its replica importer
principal and refreshes membership authority before import batches.

## Unit 5 Result: Membership E2E Contract

`p2panda-auth-membership-contract` is now part of `mvp-e2e -- all`. It proves
the durable membership store is not just a process-role wiring detail:

- root creates an island membership log;
- root adds writers and replica importers;
- active writers can write p2panda facts;
- replica importers can import active writer operations on another store;
- Ployz fact-key grants still reject a member that has membership but no write
  grant;
- Ployz fact-key grants do not authorize a non-member writer;
- replica import still checks the original fact author's write grant, not the
  importer session's read grant;
- a demoted writer loses write authority but can import as a replica importer;
- a removed writer cannot write or import fresh facts;
- a fresh operation created from a stale pre-removal authority snapshot is
  rejected by a receiver with current membership;
- a reinvited principal with a new epoch/key can write;
- the old epoch/key cannot write or import after reinvite;
- reopening the membership store replays old add/remove/reinvite operations
  without resurrecting the old key binding;
- a prod operation offered through another island is rejected;
- unauthorized imports accepted is asserted exactly zero.

The scenario emits structured counts for accepted/rejected writes/imports and
the restart/replay decision booleans. This is intentionally product-shaped:
all fact writes/imports go through `PandaFactStore` with a
`PandaFactAuthoritySource` rebuilt from `IslandAuthzStore`.

Verification:

```text
cd MVP && cargo run -p mvp-e2e -- p2panda-auth-membership-contract
```

## Unit 6 Result: Deletion And Containment Ledger

Manual p2panda trust APIs still exist, but Slice 041 moves them out of
product-shaped process-serving paths. The containment grep is now classified as
follows:

```text
cd MVP && rg "p2panda-trusted-author|p2panda-author|p2panda-author-key|TrustedP2pandaAuthor|trusted_author_keys|trusted_replica_peers|trust_replica_peer|trust_author_key" \
  p2panda-authz p2panda-facts p2panda-transport e2e/src
```

Retained deliberately:

- `p2panda-facts/src/lib.rs`: manual trust maps and APIs remain as legacy
  fallback/fixture compatibility for stores without an installed membership
  snapshot. They are not the product path after Slice 041.
- `p2panda-transport/src/tests.rs`: fixture-only setup for transport tests.
- older E2E contracts such as deploy restart recovery, sync, machine remove,
  volume transfer, and p2panda-net direct probes still use manual trust
  fixtures until each product slice migrates to membership-backed authority.

Deleted from product-shaped process roles:

- `--p2panda-author`
- `--p2panda-author-key`
- `--p2panda-trusted-author`
- `TrustedP2pandaAuthor`
- serving-role trusted-author configuration as the authority boundary.

The next migration slice should move ACME and the core p2panda sync proof onto
the same membership-backed authority shape, then classify the remaining manual
trust call sites as fixture-only or delete them.
