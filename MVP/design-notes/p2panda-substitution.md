---
title: p2panda Substrate Substitution
status: evidence-complete
created: 2026-05-18
slice: 018a
---

# p2panda Substrate Substitution

## Decision

Bias toward adoption.

Use `p2panda-core`, `p2panda-store`, and `p2panda-stream` as the next fact
substrate direction inside `MVP/`, behind the existing `FactSource` boundary.
Do not replace PloyzBus, projection reducers, deploy/machine/ACME business
logic, gateway/DNS serving, or WireGuard planning.

This is not because p2panda is proven production-perfect. It is because our
custom substrate is also not production-ready, and p2panda already owns a better
version of the generic pieces: signed operations, operation validation,
append-only logs, local stores, log sync interfaces, and CRDT-shaped group
membership.

The adoption sequence should be:

1. Replace future custom fact-envelope work with `p2panda-core::Operation`.
2. Replace local fact operation storage/indexing with `p2panda-store` plus a
   small Ployz adapter.
3. Use `p2panda-stream` ingestion for operation validation, ordering, retry, and
   local persistence.
4. Spike `p2panda-auth` for island membership and strong-removal semantics
   before adding more custom membership/revocation code.
5. Defer `p2panda-net`, `p2panda-discovery`, and `p2panda-blobs` adoption until
   the version and API fit is cleaner.

This is an adopt-next recommendation, not a claim that p2panda is final
production substrate. It must stay behind a reversible adapter until the MVP's
E2E proofs pass on top of it.

## Evidence

The retired spike crate at `MVP/p2panda-spike` compiled against crates.io
p2panda 0.5.2 and proved the original fact-substrate fit:

- a signed p2panda operation decodes into a Ployz `FactCandidate`;
- two conflicting operations for the same Ployz fact key both reach the
  projection boundary;
- fact payloads can be read by candidate content hash;
- p2panda body hashes map cleanly to the existing `b3:<hex>` content-hash
  shape;
- p2panda log storage can group operations by island while preserving author
  identity through the operation public key and Ployz principal metadata.

Slice 025 deleted the spike after `mvp-p2panda-facts` covered those behaviors
with production-shaped operation writing, trusted import, persistence, sync,
duplicate/conflict, and payload-read tests.

## Current Custom Substrate Inventory

These line counts are not exact deletion estimates. They identify where the MVP
is carrying custom substrate that is not business logic.

| Area | File | LOC | p2panda relevance |
| --- | --- | ---: | --- |
| iroh docs fact wrapper/local view | `MVP/iroh/src/facts.rs` | 1428 | Replace or shrink with p2panda operation/store adapter plus narrower iroh bridge. |
| Projection fact abstraction | `MVP/projection/src/source.rs` | 368 | Keep the public seam; feed it from p2panda-backed candidates. |
| Bus fact source | `MVP/projection/src/bus_source.rs` | 180 | Keep as harness fixture or delete after p2panda-backed tests take over. |
| Projection reducer | `MVP/projection/src/reducer.rs` | 2781 | Keep. This is Ployz business reduction, not substrate. |
| Process fact source | `MVP/e2e/src/process_fact_source.rs` | 682 | Replace as more E2E proofs use a real p2panda-backed fact source. |
| In-memory fact store/key/payload | `MVP/bus/src/facts.rs` | 677 | Shrink to test fixture or remove from production-shaped paths. |
| Grants/authority checks | `MVP/bus/src/grants.rs` | 440 | Keep subject/RPC grants; evaluate p2panda-auth for membership only. |
| Advisory leases | `MVP/lease/src/lib.rs` | 1859 | Keep lease reducer/business semantics; store lease facts as p2panda operations. |
| Mesh invite/snapshot/WG | `MVP/mesh/src/*.rs` | 710 | Keep product semantics; p2panda-auth may help membership roots/revocation. |

Likely near-term deletion/avoidance is not "delete 9k LOC." The honest target
is to prevent `MVP/iroh/src/facts.rs`, `MVP/e2e/src/process_fact_source.rs`,
and `MVP/bus/src/facts.rs` from becoming the permanent custom storage/sync
stack. The business reducers stay.

## Crate Matrix

| Crate | Current signal | Recommendation |
| --- | --- | --- |
| `p2panda-core` 0.5.2 | MIT/Apache-2.0, edition 2024, signed Ed25519 operation headers, BLAKE3 body hashes, CBOR encoding, custom extensions. | Adopt next behind a Ployz fact adapter. |
| `p2panda-store` 0.5.2 | MIT/Apache-2.0, memory store by default, SQLite feature available, read/write traits for operations and logs. Does not validate log integrity by itself. | Adopt next for local operation storage, with Ployz-owned indexing/prefix queries where needed. |
| `p2panda-stream` 0.5.2 | Ingest validates operations/backlinks, returns retry/outdated/complete states, persists through `p2panda-store`. | Adopt next for fact ingestion instead of hand-rolled operation validation. |
| `p2panda-auth` 0.5.2 | Decentralized group management, Pull/Read/Write/Manage, conditions, strong-removal resolver, eventually consistent group state. | Spike next for island membership and revocation. Bias toward adoption for membership, not subject permissions. |
| `p2panda-sync` 0.5.2 | Data-type-agnostic sync traits and append-only log sync protocols. | Defer until the fact adapter exists; then test as replacement for custom docs sync paths. |
| `p2panda-net` 0.5.2 | iroh endpoint/gossip/discovery/sync stack, but depends on `iroh = 0.96.1` and `iroh-gossip = 0.96.0` while MVP uses the 1.0-rc iroh family. | Defer. Do not introduce parallel transport stacks before fact substrate adoption. |
| `p2panda-discovery` 0.5.2 | Confidential topic/node discovery, random-walk feature. | Defer. Current MVP does not need this before real p2panda sync adoption. |
| `p2panda-blobs` 0.5.2 | Docs show a useful wrapper over `iroh-blobs`, but the crates.io package's `src/lib.rs` currently only contains a refactor TODO and exports no API. | Do not adopt from crates.io now. Recheck a future release or use direct `iroh-blobs`. |

## Why Not Replace The Bus

PloyzBus is NATS-core-shaped:

- subjects and wildcards;
- no-responder request/reply;
- request-many;
- queue groups;
- service registry;
- drain;
- authority-island bridge imports/exports;
- subject/fact/RPC grants.

p2panda's networking stack is closer to local-first broadcast/sync. That is
useful below facts and possibly below membership. It is not a replacement for
the public PloyzBus primitive.

## What p2panda Should Replace

Replace generic substrate:

- signed fact envelope;
- author public-key validation;
- operation body hash validation;
- append-only per-author logs;
- local operation storage;
- out-of-order ingestion handling;
- future group membership/revocation CRDTs;
- possibly future log sync once the adapter is stable.

Keep Ployz-owned semantics:

- subject bus;
- command entry conflicts;
- deterministic projection reducers;
- deploy commit-before-drain;
- ACME lease/challenge semantics;
- machine remove semantics;
- gateway/DNS snapshots;
- WireGuard snapshot application;
- operator-visible status and failure audiences.

## Spike Findings

### Fact Operation Fit

`p2panda-core::Header` extensions can carry the Ployz metadata that does not
belong in the body:

- island;
- fact key;
- principal id;
- future fact kind/epoch shortcuts if useful.

The operation body carries the fact payload. The body hash becomes the existing
Ployz content hash. This preserves the current projection reducer contract:
reducers still receive `FactCandidate` values and still decide winners by Ployz
business rules such as `(epoch desc, content_hash asc)`.

The adapter does need a Ployz index. `p2panda-store` groups logs by author and
log id; it does not provide Ployz fact-prefix queries by itself. That is fine as
long as the index remains small and explicit. We should not pretend p2panda
eliminates all application indexing.

### Auth Fit

`p2panda-auth` is the strongest candidate after core/store/stream. Its strong
removal semantics are directly relevant to machine removal and future
re-invite behavior.

Do not use it for PloyzBus subject grants. The access levels and conditions can
probably model dataset/fact access, but the bus needs subject wildcards,
temporary response permission, queue permissions, and bridge import/export
rules. Those remain Ployz-specific.

### Sync And Network Fit

`p2panda-sync` should be evaluated after a real p2panda-backed fact adapter
exists. `p2panda-net` is premature because its iroh dependency line is behind
the MVP's current iroh 1.0-rc family, so adopting it now would introduce a
second iroh stack.

This does not mean "never." It means "not before the fact substrate seam is
converted."

### Blob Fit

The p2panda blobs idea is right, but the crates.io package is not currently
usable as a replacement. Its published 0.5.2 library exports no blob API. Keep
using direct iroh-blobs or revisit p2panda-blobs when a usable release ships.

## Recommended Next Slice Rewrite

Before implementing deploy restart recovery, insert a substrate slice:

```text
Slice 018b: p2panda-backed fact substrate
```

Target:

- introduce an MVP-local p2panda fact adapter, probably replacing the spike
  crate with production-shaped code;
- preserve `FactSource` as the projection-facing seam;
- keep reducers unchanged;
- move docs-backed ACME and machine-remove proofs toward the p2panda-backed
  fact source where practical;
- leave transport sync as local/in-memory first if needed, then add sync later.

Success criteria:

- existing projection reducers consume p2panda-backed candidates;
- conflict candidates remain visible;
- payload absence is explicit;
- unauthorized authors remain explicit;
- SQLite projection rebuild still works;
- the adapter removes or prevents enough custom fact-envelope code to justify
  the dependency.

The deploy restart recovery slice should then build on this substrate instead
of hardening the current custom iroh-docs wrapper further.

## Slice 018b Outcome

Slice 018b added `MVP/p2panda-facts` as the production-shaped adapter:

- local writes create p2panda operations with Ployz island/key/principal
  metadata in header extensions;
- `p2panda-stream` ingests and validates the append-log operation;
- `FactAuthorizer` still gates writes and reads;
- projection consumes `FactCandidate` values through the existing `FactSource`
  trait;
- `p2panda-fact-source-contract` proves existing reducers rebuild SQLite and
  gateway/DNS snapshots from p2panda-backed candidates.

The spike crate has been retired. New fact-substrate work should target
`mvp-p2panda-facts`.

## Open Questions

- Should Ployz principal identity become the p2panda public key, or remain a
  Ployz principal string mapped to a p2panda public key? The spike keeps both.
  Production probably wants explicit principal-to-key membership facts.
- Should the Ployz fact key live only in header extensions, or should the log id
  also include a fact-key prefix to make scans cheaper? Start simple; add an
  index only when measurement says prefix scans hurt.
- How much of `p2panda-auth` can carry fact-write membership before subject/RPC
  grants become clearer as a separate Ployz layer? Spike this before adding
  more custom revocation code.

## Primary Sources

- p2panda repository and stability warning:
  <https://github.com/p2panda/p2panda>
- p2panda 2024 rewrite and breaking-change note:
  <https://p2panda.org/2024/12/06/p2panda-release.html>
- `p2panda-core` crate:
  <https://docs.rs/crate/p2panda-core/latest>
- `p2panda-store` crate:
  <https://docs.rs/crate/p2panda-store/latest>
- `p2panda-stream` crate:
  <https://docs.rs/crate/p2panda-stream/latest>
- `p2panda-auth` crate:
  <https://docs.rs/p2panda-auth/latest/p2panda_auth/>
- `p2panda-net` crate:
  <https://docs.rs/p2panda-net/latest/p2panda_net/>
- `p2panda-discovery` crate:
  <https://docs.rs/crate/p2panda-discovery/latest>
- `p2panda-blobs` crate:
  <https://docs.rs/p2panda-blobs/latest/p2panda_blobs/>
