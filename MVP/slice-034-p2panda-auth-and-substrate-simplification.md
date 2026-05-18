---
title: Slice 034 p2panda Auth And Substrate Simplification
status: complete
created: 2026-05-18
plan: MVP/slice-034-p2panda-auth-and-substrate-simplification-plan.md
---

# Slice 034 p2panda Auth And Substrate Simplification

## Result

`p2panda-auth` is a good fit for Ployz island membership and revocation, with
one important boundary: it does not authenticate Ployz membership operations by
itself. Ployz still needs signed, island-scoped membership operations before
p2panda-auth state can replace `PandaFactStore` trust maps.

This slice adds `mvp-p2panda-authz` as a compile-backed membership spike. It is
not product-wired yet, but it proves the group-state semantics we need:

- root-created island group;
- manager-only add/remove/demote;
- stable member binding `(island, principal, epoch, p2panda public key)` in the
  adapter read model;
- test-only signed membership-operation validation using a p2panda private key
  verified against the bound public key, plus wrong-group, missing introduced
  binding, unsupported nested group, and rejected-import state-preservation
  checks;
- replica importer as Pull/Read plus `ReplicaImporter` condition, not Write;
- strong-removal concurrency cases for removed managers and mutual removals.

Disposition: keep for one adoption slice. The next implementation slice should
turn the signed-operation sketch into durable product data, prove replay
rebuilds key bindings, then wire an authority snapshot into
`mvp-p2panda-facts`. If that does not land, delete the spike rather than keeping
a parallel auth layer.

## Fact Store Substitution Design

Current custom trust in `mvp-p2panda-facts`:

- `trusted_author_keys: BTreeMap<(IslandId, PrincipalId), PublicKey>`
- `trusted_replica_peers: BTreeSet<(IslandId, PrincipalId)>`
- manual `PandaFactSyncScope::trusted_authors`

Next shape:

```text
signed durable membership operations
  -> mvp-p2panda-authz IslandAuthz reduction
  -> epoch/dependency-aware IslandAuthorityView
  -> PandaFactStore import/write checks
```

`PandaFactStore` should stop owning membership truth. It should receive an
authority view answering questions at the membership dependency point claimed by
the fact operation, not just "latest state" questions:

- was this principal a writer for this island at the referenced membership
  operation/epoch?
- does this principal's bound p2panda public key match the operation author?
- was this principal an active replica importer at the referenced membership
  operation/epoch?
- is this author removed or demoted before, concurrent with, or after the fact
  operation's referenced membership state?

PloyzBus grants and command-specific fact-key grants remain separate. A member
with p2panda-auth Write is not automatically allowed to write every Ployz fact
key.

## Deletion Readiness

| Candidate | Decision | Gate |
| --- | --- | --- |
| `trusted_author_keys` in `PandaFactStore` | Replace next | Persist/reopen membership operations and use an epoch/dependency-aware authz view for local writes, import, sync scope, and process-role reopen. |
| `trusted_replica_peers` in `PandaFactStore` | Replace next | Model replica import as active Pull/Read member with `ReplicaImporter`, and prove replica cannot write. |
| `PandaFactSyncScope::from_trusted_authors` | Replace after authz wiring | Sync scope should derive from active authz members instead of manually seeded key maps. |
| `BusFactSource` | Keep as unit fixture | Stop expanding product proofs on it; product command scenarios should use domain p2panda writers or `SharedPandaFactStore`. |
| direct `BusActorHandle::write_fact_payload` in E2E | Keep only for bus/projection fixtures | Product-shaped deploy/serving/machine scenarios should move to domain p2panda writers before deletion. |
| `MVP/e2e/src/process_fact_source.rs` | Keep for now | Delete only after p2panda process-role paths cover every process-source proof and no product E2E imports it. |
| `MVP/iroh/src/facts.rs` | Park | Do not harden. Retire product proof use after p2panda-backed scenarios cover conflict candidates, unauthorized status, missing payload, and projection rebuild. |
| `PandaFactWireEnvelope` | Keep for now | It is a small stable Ployz frame over p2panda operations. Replace only if p2panda exposes a simpler raw operation payload path through the transport node. |
| `PandaNetQuarantineLog` | Keep for now | It is p2panda-net adapter glue, not product semantics. Revisit when fact-node delivery can append directly to the canonical p2panda store without wrapper operations. |
| `p2panda-blobs` | Do not adopt | crates.io `0.5.2` still does not expose a usable library API. |

## Code Size

New code:

- `mvp-p2panda-authz`: 21 compile-backed tests and a narrow adapter over
  p2panda-auth group state.

Reviewer caveat addressed: the spike no longer uses the p2panda-auth operation
id as a plain local counter. The local sequence is only hash input. The
production adoption slice should still derive membership operation ids from the
durable signed p2panda operation hash rather than treating the spike id format
as a wire contract.

Deletion potential identified:

- `MVP/p2panda-facts/src/lib.rs`: manual trust maps and sync-scope construction.
- `MVP/e2e/src/process_fact_source.rs`: fixture-only once p2panda process roles
  cover the remaining process proofs.
- `MVP/iroh/src/facts.rs`: historical proof path, not the durable fact
  direction.

This slice intentionally does not delete those paths. It names proof gates so a
follow-up slice can remove code without changing product semantics.

## Verification

```text
cargo fmt --manifest-path MVP/Cargo.toml -p mvp-p2panda-authz
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-authz --all-targets
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-p2panda-authz --all-targets -- -D warnings
cargo check --manifest-path MVP/Cargo.toml --workspace --all-targets
cargo info p2panda-blobs --verbose
```
