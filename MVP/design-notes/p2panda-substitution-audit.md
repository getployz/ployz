---
title: p2panda Substitution Audit
status: completed
created: 2026-05-18
slice: 019a
origin:
  - MVP/slice-019a-p2panda-substitution-audit-plan.md
  - MVP/design-notes/p2panda-substitution.md
  - MVP/slice-018b-p2panda-fact-substrate.md
  - MVP/slice-018c-p2panda-deploy-restart-recovery.md
---

# p2panda Substitution Audit

## Decision

Move one more substrate slice ahead of ACME: persistent p2panda fact storage,
derived-index rebuild, and fact-store role restart.

ACME should still be the next product canary after that. Running ACME now would
prove advisory leases and serving behavior on top of an in-memory p2panda fact
role. That would harden the wrong boundary. The daemon/data-plane invariant
needs the fact substrate to survive role restart before another product feature
depends on it.

The strongest substitution path is:

1. Keep `FactSource` as the Ployz projection-facing seam.
2. Make `mvp-p2panda-facts` persistent with `p2panda-store` SQLite support.
3. Rebuild every Ployz index from p2panda operations at open time.
4. Replace process-role JSON fact durability with a p2panda fact-store role.
5. Only then move ACME leases/challenges onto the p2panda fact boundary.
6. Spike `p2panda-auth` for island membership before adding more custom
   membership/revocation logic.
7. Evaluate `p2panda-sync` after persistent stores exist on both sides.

This keeps the bias toward p2panda without pretending it owns Ployz business
semantics.

## External Grounding

Primary sources checked during this audit:

- `p2panda-core` 0.5.2 provides signed append-only operations, BLAKE3 body
  hashes, custom header extensions, fork tolerance, pruning hooks, and partial
  sync support: <https://docs.rs/crate/p2panda-core/latest>.
- `p2panda-store` 0.5.2 provides read/write store traits, Memory and SQLite
  stores, and an explicit atomic transaction model. It does not remove the need
  for Ployz-owned derived indexes: <https://docs.rs/crate/p2panda-store/latest>.
- `p2panda-stream` 0.5.2 provides stream helpers to decode, validate, order,
  prune, and store p2panda operations: <https://docs.rs/p2panda-stream/latest/p2panda_stream/>.
- `p2panda-auth` 0.5.2 provides eventually consistent group state, Pull/Read/
  Write/Manage access levels, strict group modification, and a strong-removal
  conflict resolver: <https://docs.rs/p2panda-auth/latest/p2panda_auth/>.
- `p2panda-sync` 0.5.2 provides data-type-agnostic sync managers/protocols for
  append-only logs, with low documentation coverage and a lower-level API:
  <https://docs.rs/p2panda-sync/latest/p2panda_sync/>.
- `p2panda-net` remains deferred because it would introduce a broader local-
  first networking stack and currently sits on a different iroh line than this
  MVP's transport direction.

## Substitution Ledger

| Area | Current code | Keep/replace/defer | Reason | Proof before deletion |
| --- | --- | --- | --- | --- |
| Projection seam | `MVP/projection/src/source.rs` | Keep | Reducers need a stable Ployz-facing view: island, fact key, author, content hash, status, payload reads. p2panda should feed this seam, not leak into every reducer. | Existing projection contracts continue through `FactSource`. |
| Signed operation envelope | `MVP/p2panda-facts/src/lib.rs`, older `MVP/iroh/src/facts.rs` envelope logic | Replace custom paths with p2panda | `p2panda-core` and `p2panda-stream` already validate signatures, body hashes, append-log ordering, duplicates, and retry/outdated ingestion states better than bespoke code. | Bad signature/body/hash, duplicate, conflict, out-of-order, revoked/untrusted author, and cross-island import tests. |
| Local operation storage | `MVP/p2panda-facts/src/lib.rs`, `MVP/e2e/src/process_fact_source.rs`, `MVP/bus/src/facts.rs` | Replace for production-shaped paths | `p2panda-store` has SQLite and transaction primitives. Our process JSON store and bus fact store should not become production substrate. | Reopen a SQLite p2panda store, rebuild indexes, project serving state, import duplicate/conflict operations after reopen. |
| Derived fact index | current in-memory indexes in p2panda, bus, iroh docs, process source | Keep Ployz-owned, rebuildable | p2panda stores logs by protocol needs; Ployz needs prefix/key/kind/status reads for reducers. The index is acceptable only if it is derived from operations. | Delete derived index, rebuild from operations, get byte-identical projection outputs. |
| Process fact source | `MVP/e2e/src/process_fact_source.rs` | Replace after persistence slice | It proves process fate separation today, but JSON entry/blob files are another custom fact store. | Process-role E2E uses p2panda persistent fact role; kill/restart fact role; serving/projection recover. |
| Bus fact store | `MVP/bus/src/facts.rs`, `MVP/projection/src/bus_source.rs` | Shrink to fixture, then delete from product proofs | Useful harness for early bus/projection proofs; not the durable fact direction. | Projection, deploy, serving, scale, and machine scenarios use p2panda or an explicitly named test fixture. |
| Iroh docs fact source | `MVP/iroh/src/facts.rs` | Park, then delete or shrink to transport bridge | It is the largest remaining custom fact local-view wrapper. p2panda now owns operation envelope/storage; iroh-docs should not be hardened in parallel. | Port `iroh-docs-contract` semantics to p2panda persistence/sync: conflict candidates, unauthorized/unverified status, missing payload, projection rebuild. |
| p2panda spike crate | `MVP/p2panda-spike/src/lib.rs` | Delete after persistent adapter proof | It has served its purpose as compile evidence. Keeping it risks two examples diverging. | `mvp-p2panda-facts` covers every spike behavior plus persistence. |
| Operation export/import | `MVP/p2panda-facts/src/lib.rs` | Keep narrow until sync proof | Manual exchange is acceptable for deterministic local E2E. It is not a production sync protocol. | Persistent two-store proof first; then p2panda-sync two-party proof with offline catch-up. |
| Membership/revocation | `MVP/mesh`, `MVP/machine`, bus grants | Spike `p2panda-auth` for membership only | Strong removal and eventually consistent group state map to island membership. Subject permissions, queue permissions, response permissions, and bridge imports/exports remain Ployz bus semantics. | Root add/remove/demote, concurrent remove/re-add, tombstone domination, and WireGuard projection tests. |
| Advisory leases | `MVP/lease/src/lib.rs` | Keep reducer semantics; store as p2panda facts | Lease behavior is Ployz product semantics: TTL, renewal, epoch fencing, supersession, visible-node context, RAII release. p2panda can store signed facts, not decide lease policy. | ACME p2panda HTTP-01 contract after persistence. |
| PloyzBus | `MVP/bus/src/*.rs` | Keep | p2panda does local-first sync/eventing. It does not replace NATS-shaped request/reply, no responders, request-many, queue groups, service registry, drain, subject grants, or bridge semantics. | Existing bus contracts and future iroh transport proof. |
| Reducers/business logic | deploy, ACME, machine, projection reducers | Keep | This is the product. The rewrite exists to make this code smaller and clearer, not to outsource it to a substrate crate. | Semantic-leverage reports against old deploy/ACME/machine code. |

## Code Pressure

Current custom substrate pressure in the MVP workspace is concentrated in a few
files:

| File | Current LOC | Audit position |
| --- | ---: | --- |
| `MVP/iroh/src/facts.rs` | 1689 | Largest custom fact wrapper. Do not add new product behavior on top of it. |
| `MVP/e2e/src/process_fact_source.rs` | 682 | Useful process-fate proof, but should be replaced by persistent p2panda fact role. |
| `MVP/bus/src/facts.rs` | 677 | Keep as bus/test fixture until p2panda-backed proofs replace all production-shaped reads. |
| `MVP/projection/src/bus_source.rs` | 180 | Fixture adapter; delete when no E2E scenarios need bus facts. |
| `MVP/p2panda-spike/src/lib.rs` | 396 | Delete after persistent p2panda adapter covers spike behavior. |

The honest near-term gain is not "delete thousands of lines immediately." It is
to stop writing new feature code against these old paths and to make the next
substrate slice unlock deletion safely.

## Next Slice

Plan and implement:

```text
Slice 019b: persistent p2panda fact store and restartable fact role
```

Minimum proof:

- `PandaFactStore` can use a persistent p2panda SQLite-backed store.
- On open, it rebuilds all Ployz derived indexes from p2panda operations.
- Duplicate, conflict, out-of-order, untrusted-key, revoked-author, missing
  payload, and cross-island cases still produce structured outcomes.
- A process-role E2E can kill/restart the fact role without losing fact truth.
- Projection rebuilds SQLite and gateway/DNS snapshots from the reopened
  p2panda operation log.
- Serving keeps last-good state while the coordinator is down and while the
  fact role is restarted.
- The old `ProcessFactSource` is either deleted from at least one E2E path or
  explicitly marked as a legacy fixture with no new users.

Target commands:

```text
cd MVP && cargo test -p mvp-p2panda-facts --lib
cd MVP && cargo run -p mvp-e2e -- p2panda-fact-source-contract
cd MVP && cargo run -p mvp-e2e -- deploy-restart-recovery-contract
cd MVP && cargo run -p mvp-e2e -- process-role-contract
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

After that, unpark ACME as:

```text
Slice 020: p2panda-backed ACME HTTP-01 with advisory lease facts
```

## p2panda-auth Plan

Do not fold `p2panda-auth` into Slice 019b. Persistence is the blocker for
deleting custom fact-store paths; auth is the next membership blocker.

The auth spike should map:

- authority island root/admin principal -> group member with Manage;
- node principal -> group member with Write or a Ployz-specific condition;
- read-only runtime principal -> Read/Pull where useful;
- machine remove -> strong-removal group operation plus existing Ployz
  tombstone fact;
- reinvite -> explicit new epoch/key path, not resurrection by accident.

Adopt it if it reduces custom membership and revocation rules while preserving
Ployz command entry checks. Reject it for PloyzBus subject permissions unless a
future proof shows conditions can express wildcard subjects, queue permissions,
temporary reply permissions, and bridge imports/exports cleanly.

## p2panda-sync Plan

Do not adopt `p2panda-sync` before persistent stores exist. Sync without
persistent reopen/index rebuild would prove less than the current manual
export/import path.

Evaluate it after Slice 019b with:

- two persistent stores over a test transport;
- offline catch-up after one side misses operations;
- duplicate and out-of-order idempotency;
- latency/lag metrics at 200, 1,000, and 10,000 synthetic fact counts.

Prefer direct `p2panda-sync` before `p2panda-net`. `p2panda-net` brings a wider
networking runtime and still does not express PloyzBus request/reply semantics.

## What Not To Substitute

Keep these Ployz-owned:

- NATS-shaped bus semantics;
- authority-island bridge/import/export rules;
- subject/fact/RPC grants above membership;
- advisory lease reducer and command conflict behavior;
- deterministic projection reducers;
- deploy commit-before-drain;
- ACME challenge ownership and serving projection;
- machine remove behavior;
- WireGuard snapshot planning/application;
- serving last-good state and validated reload semantics.

The goal is substrate deletion, not product semantic deletion.

## Verification

This slice changed planning and decision documents only. The uncommitted
p2panda implementation/test spike files in the working tree were intentionally
not part of this audit commit.

Run before shipping this report:

```text
git diff --check -- MVP/design-notes/p2panda-substitution-audit.md MVP/overall-plan.md MVP/primitive-decisions.md MVP/slice-019a-p2panda-substitution-audit-plan.md MVP/slice-019-p2panda-acme-http01-plan.md
```

No cargo tests are required for the docs-only audit commit. The next
implementation slice must run the p2panda and E2E gates listed above.
