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
