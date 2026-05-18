---
title: Slice 037 p2panda 0.6 Substrate Deletion
status: completed
created: 2026-05-19
plan: MVP/slice-037-p2panda-06-substrate-deletion-plan.md
---

# Slice 037 p2panda 0.6 Substrate Deletion

## Result

`MVP/p2panda-06-spike` is a compile-backed proof against crates.io p2panda
`0.6.0`. It is intentionally an excluded nested workspace because the current
MVP workspace cannot resolve both:

- existing `mvp-iroh` -> `iroh 0.96.1` -> `iroh-base 0.96.1` ->
  `ed25519-dalek =3.0.0-pre.1`;
- `p2panda-net 0.6.0` -> `iroh 0.98.2` -> `ed25519-dalek =3.0.0-pre.6`.

That means p2panda-net `0.6.0` is usable, but not as a side-by-side workspace
member while the historical direct-iroh proof remains pinned to `0.96`. The
production migration should align the MVP iroh line to `0.98` or retire the
historical `mvp-iroh` proof before adopting p2panda-net `0.6` in the active
workspace.

## Compile Evidence

The spike proves:

- `Operation<PloyzFactExtensions>` can carry a Ployz fact directly, with island,
  fact key, principal, authority epoch, content hash, kind, and log id in
  p2panda header extensions and payload bytes in the operation body.
- `RawOperation` is sufficient for encode/decode of the canonical fact operation;
  no `PFO1` frame is needed for the success path.
- p2panda validation catches header/body tampering, and Ployz can still add a
  structured content-hash mismatch classification above it.
- `p2panda-store 0.6.0` SQLite implements the operation, log, topic, and group
  store traits needed for canonical fact operations.
- `p2panda-net::sync::LogSync<SqliteStore, PloyzLogId, PloyzFactExtensions>`
  type-checks, so the transport can move canonical fact operations instead of
  wrapper operations.
- `p2panda-auth 0.6.0` `GroupsProcessor` persists group state with Ployz-owned
  conditions such as `FactWriter`. `IslandAuthoritySnapshot` remains the right
  seam; p2panda owns group reduction, not Ployz principal/key/epoch binding or
  fact-key grants.
- `p2panda-blobs 0.5.2` is not an adoption target. Its crate root still only
  contains the upstream note that it needs refactoring after the p2panda-net
  refactor.

The spike's `PloyzFactExtensions`, `PloyzLogId`, `AuthzStateId`, and
`FactKind` are compile-only stand-ins. Slice 038 must map the canonical
operation shape to existing MVP identity/projection types rather than promote
these local structs into production.

## Deletion Ledger

| Area | Classification | Evidence | Next step |
| --- | --- | --- | --- |
| `PandaFactWireEnvelope` / `PFO1` | Candidate delete after migration gates | The spike round-trips canonical facts as `Operation<PloyzFactExtensions>` plus `RawOperation`. | Move live p2panda-net transport to canonical operations, prove branchable duplicate/conflict/malformed/wrong-author/unauthorized outcomes, then delete `PFO1` from success paths. |
| `PandaNetQuarantineLog` | Candidate delete after migration gates | `LogSync<SqliteStore, PloyzLogId, PloyzFactExtensions>` type-checks against p2panda-net `0.6.0`. | Use one canonical p2panda operation log, prove live canonical sync and oversized/malformed/unauthorized reporting, then delete wrapper-log signing. |
| Wrapper operation replay cache | Candidate delete after migration gates | Canonical fact operations have one operation hash; duplicate classification can happen at the p2panda/Ployz fact-store boundary. | Replace wrapper hash suppression only after canonical operation deduplication tests cover stream replay. |
| Manual trusted-author fallback | Keep as fixture until product callers are gone | Slice 035 already moved the product path to `IslandAuthoritySnapshot`; grep still shows E2E/process fixtures seeding manual trusted authors. | Move remaining product-shaped E2Es to authz snapshots, then hide/delete fallback APIs. |
| Manual trusted-replica fallback | Keep as fixture until product callers are gone | The authz seam supports replica importer authority, but E2Es still seed trusted replica sessions manually. | Require authz-derived replica authority on product paths. |
| `IslandAuthoritySnapshot` | Keep product-owned | p2panda-auth proves group state; Ployz still needs root/admin anchoring, principal/key/epoch binding, and fact-key grants. | Keep the seam while simplifying the underlying authz store during the 0.6 migration. |
| `iroh-docs-contract` | Retire after equivalent p2panda 0.6 migration | p2panda-net/store/sync cover the durable append-log direction; current iroh-docs line blocks side-by-side p2panda-net 0.6 in the workspace. | Park as historical proof or remove from `mvp-e2e all` once canonical p2panda-net transport covers conflict/unauthorized/rebuild semantics. |
| Process JSON fact source | Retire after process p2panda proof covers same fate case | Persistent p2panda stores and p2panda-net process serving already cover most process-role durability. | Delete only after verifying no unique daemon-down serving proof remains. |
| Bus fact store | Keep as fixture | The bus still needs cheap local tests. It is not the durable fact product path. | Name fixture-only status in docs; avoid product proofs depending on it. |
| p2panda-blobs | Defer | Current published crate is effectively a placeholder after the p2panda-net refactor. | Revisit only when an API compatible with p2panda-net `0.6+` ships. |

## Proof Path Classification

Keep in `mvp-e2e all` until the migration slice replaces them:

- `p2panda-net-process-serving-contract`;
- product canaries using p2panda-backed facts: ACME, deploy, machine, volume,
  environment, serving, and scale.

Replace during the 0.6 migration:

- `p2panda-net-sync-contract`;
- `p2panda-net-owned-node-contract`;
- `p2panda-net-fact-node-contract`.

These should move from opaque `PandaFactWireEnvelope` bodies to canonical
`Operation<PloyzFactExtensions>` transport.

Park or retire after the replacement exists:

- `iroh-docs-contract`;
- `process-role-serving-contract` if the p2panda process-serving proof covers
  the same daemon-down/serving-last-good behavior;
- process JSON fact source harnesses that only exist to emulate persistence now
  covered by p2panda SQLite stores.

## Next Slice

The next implementation slice should be:

```text
Slice 038: p2panda 0.6 canonical fact transport migration
```

Scope:

1. Align the MVP p2panda/iroh line to crates.io p2panda `0.6.0` and iroh
   `0.98`, or remove/park the old direct-iroh proof from the active workspace
   first.
2. Change `mvp-p2panda-transport` to publish canonical
   `Operation<PloyzFactExtensions>` values.
3. Replace `PandaNetQuarantineLog` with p2panda-store topic/log usage.
4. Keep `FactSource`, `IslandAuthoritySnapshot`, and structured Ployz import
   outcomes as stable product seams.
5. Prove duplicate, conflict, malformed, oversized, wrong-author, cross-island,
   and unauthorized outcomes remain branchable.
6. Re-run the p2panda-net E2Es and the process-serving canary before deleting
   old envelope paths.

## Verification

```text
cd MVP && cargo fmt --all -- --check
cd MVP && cargo fmt --manifest-path p2panda-06-spike/Cargo.toml -- --check
cd MVP && cargo check --manifest-path p2panda-06-spike/Cargo.toml
cd MVP && cargo test --manifest-path p2panda-06-spike/Cargo.toml
cd MVP && cargo tree --manifest-path p2panda-06-spike/Cargo.toml -i iroh
cd MVP && cargo tree --manifest-path p2panda-06-spike/Cargo.toml -i p2panda-net
cd MVP && cargo tree --manifest-path p2panda-06-spike/Cargo.toml -i p2panda-auth
```

Observed spike tests:

```text
canonical_fact_operation_round_trips_as_raw_operation
canonical_fact_rejects_content_hash_tampering
sqlite_store_supports_operation_log_and_topic_traits_for_canonical_facts
p2panda_net_log_sync_accepts_canonical_fact_store_shape
p2panda_auth_processor_persists_group_state_with_conditions
```
