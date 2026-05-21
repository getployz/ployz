---
title: Slice 018b p2panda Fact Substrate Report
status: completed
created: 2026-05-18
plan: MVP/slice-018b-p2panda-fact-substrate-plan.md
---

# Slice 018b p2panda Fact Substrate Report

## Result

Slice 018b added `mvp-p2panda-facts` as the first production-shaped fact
substrate adapter under `MVP/`.

The adapter uses:

- `p2panda-core` for signed operation headers, body hashes, and validation;
- `p2panda-store` for local operation/log storage;
- `p2panda-stream` for ingest validation and append-log persistence;
- the existing Ployz `FactAuthorizer` for write/read grants;
- the existing projection `FactSource` boundary for reducers.

The business reducers did not change. Existing projection code can now rebuild
SQLite and gateway/DNS snapshots from p2panda-backed fact candidates.

## What p2panda Owns Now

- Signed fact envelopes.
- Payload hash binding.
- Author append-log sequence/backlink validation.
- Local operation ingestion.
- Local operation storage.

## What Ployz Still Owns

- Subject bus semantics.
- Fact grants and island authorization.
- Mapping p2panda public keys to Ployz principals.
- Candidate statuses exposed to reducers.
- Deterministic projection reducers.
- Gateway/DNS snapshot semantics.
- Deploy, ACME, lease, membership, and routing business logic.

## Current Adapter Shape

`PandaFactStore` writes local p2panda operations and keeps a small projection
index in memory so it can implement the current synchronous `FactSource` trait.
That index is derived state, not durable truth. Durable truth is the p2panda
operation log. A future persistent/sync slice should rebuild the index from
p2panda operations instead of treating it as authority.

Writes are session-bound. A caller cannot construct a `PandaFactAuthor` for a
privileged principal and pass authorization by name; the session principal must
match the author principal before the adapter checks write grants.

`PandaFactWriteOutcome` mirrors the immutable fact contract:

- `Inserted` for the first content hash at a fact key;
- `AlreadyPresent` for the same key and content hash;
- `Conflict` for the same key and a different content hash.

Conflicts are retained as candidates rather than overwritten.

## E2E Proof

Added `p2panda-fact-source-contract`.

The scenario:

1. writes node, service, route, gateway, DNS, and conflicting fact candidates
   through `mvp-p2panda-facts`;
2. runs the existing projection actor against the p2panda-backed `FactSource`;
3. verifies projected node/service/gateway/DNS state;
4. deletes SQLite and rebuilds deterministically;
5. verifies gateway/DNS snapshot loading;
6. verifies conflict candidates surface through projection status.

## Follow-Up Work

- Move deploy restart recovery to the p2panda-backed fact boundary.
- Move remaining docs-backed proof paths only when the p2panda-backed
  equivalent exists.
- Add p2panda import/sync APIs and then prove unknown-author, cross-island,
  missing-payload, payload-mismatch, and index-rebuild behavior.
- Spike `p2panda-auth` for island membership and principal/key binding.
- Evaluate `p2panda-sync` after the local adapter has replaced enough custom
  source/writer paths to make sync integration meaningful.
- Decide whether to delete `MVP/p2panda-spike` after the next slice no longer
  needs it as comparison evidence.
