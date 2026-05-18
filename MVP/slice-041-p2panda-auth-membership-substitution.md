---
title: Slice 041 p2panda-auth Membership Substitution
status: active
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
