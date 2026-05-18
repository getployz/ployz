---
title: Slice 019a p2panda Substitution Audit Plan
status: planned
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution.md
  - MVP/slice-018b-p2panda-fact-substrate.md
  - MVP/slice-018c-p2panda-deploy-restart-recovery.md
---

# Slice 019a p2panda Substitution Audit Plan

## Problem Frame

The MVP now has a p2panda-backed fact path, but the codebase still carries
several older substrate paths:

- iroh-docs fact wrappers and local views,
- bus-backed fact storage,
- process-role fact-source harnesses,
- E2E-local p2panda writer adapters,
- in-memory lease books and read-only lease projection helpers,
- custom membership/grant logic that may overlap with `p2panda-auth`,
- manual operation export/import standing in for real sync.

The operator's concern is maintenance burden: if p2panda has already solved a
generic substrate problem, prefer substituting it early over growing our own AI
written stack. This slice should produce the hard map for that decision before
the next product proof adds more code on top.

This is an investigation slice. It should not move product behavior yet. It
should identify the highest-leverage substitutions, prove or reject them with
small compile-tested spikes where needed, and update the plan map so the next
implementation slice deletes or avoids custom plumbing deliberately.

## Requirements Trace

- `VISION.md`: Ployz should expose explicit primitive operations, keep the data
  plane alive without the coordinator, and avoid hidden control-plane
  complexity.
- `MVP/overall-plan.md`: before each slice, scout crates that can replace
  substrate plumbing; lean on maintained crates for generic substrate and keep
  business semantics in Ployz code.
- `MVP/primitive-decisions.md`: p2panda now owns signed operation envelopes,
  validation, ingestion, and local operation storage for the p2panda-backed
  path; Ployz owns bus semantics, island grants, reducers, and product logic.
- `MVP/slice-018b-p2panda-fact-substrate.md`: follow-up work includes
  persistent/sync behavior, index rebuilds, p2panda-auth, and deciding whether
  the spike crate still earns its place.
- `MVP/slice-018c-p2panda-deploy-restart-recovery.md`: the next simplification
  targets are typed fact writer helpers, reducer composition, and moving
  process-role pieces out of E2E when they become production roles.
- User direction: bias toward p2panda because our bespoke MVP substrate is also
  not production-ready, and poorly maintained AI-written plumbing is worse than
  a maintained pre-1.0 crate behind an adapter.

## Current External Grounding

Primary sources checked before writing this plan:

- p2panda repository: modular crates, raw-byte compatibility, iroh/BLAKE3/
  Ed25519/CBOR foundations, broadcast-first design, and explicit pre-1.0 API
  instability warning: <https://github.com/p2panda/p2panda>.
- `p2panda-core` 0.5.2: signed append-only operations, custom header
  extensions, body hashes, single-writer logs, fork tolerance, partial sync,
  pruning hooks, MIT/Apache license: <https://docs.rs/crate/p2panda-core/latest>.
- `p2panda-store` 0.5.2: read/write store traits, memory and SQLite stores,
  atomic transaction pattern, and explicit separation between persistence and
  validation: <https://docs.rs/crate/p2panda-store/latest>.
- `p2panda-stream` 0.5.2: stream helpers to decode, validate, order, prune, and
  store p2panda operations: <https://docs.rs/p2panda-stream/latest/p2panda_stream/>.
- `p2panda-auth` 0.5.2: eventually consistent group state, Pull/Read/Write/
  Manage levels, strict group modification, DAG operations, and strong-removal
  conflict resolver: <https://docs.rs/crate/p2panda-auth/latest>.
- `p2panda-sync` 0.5.2: lower-level two-party sync traits and append-only log
  sync managers/protocols: <https://docs.rs/p2panda-sync/latest/p2panda_sync/>.
- `p2panda-net` 0.5.2: event delivery, discovery, gossip, local-first sync, and
  supervision; currently depends on `iroh` 0.96 while MVP uses the iroh 1.0-rc
  family: <https://docs.rs/p2panda-net/latest/p2panda_net/>.
- `p2panda-discovery` 0.5.2: confidential topic/node discovery using PET/PSI:
  <https://docs.rs/crate/p2panda-discovery/latest>.
- `p2panda-blobs` 0.5.2: conceptually relevant networked blob store, but the
  published crate has 0% docs and should not be adopted before a compile spike:
  <https://docs.rs/crate/p2panda-blobs/latest>.

## Scope

In scope:

- Build a current inventory of custom MVP substrate code by file and behavior.
- Decide which p2panda crates can replace or shrink each substrate area.
- Add small compile-tested spikes only when documentation is insufficient to
  decide.
- Produce deletion/shrink estimates with confidence levels and prerequisite
  tests, not optimistic promises.
- Decide whether the next implementation slice should be ACME, persistent
  p2panda fact store/index rebuild, p2panda-auth membership, or p2panda-sync.
- Update the architecture/decision docs so future slice plans stop hardening
  known throwaway paths.

Out of scope:

- Migrating product behavior during the audit.
- Replacing PloyzBus with p2panda-net.
- Replacing reducers, deploy semantics, ACME semantics, WireGuard planning, or
  gateway/DNS snapshot semantics.
- Real production network sync or discovery.
- Pingora or hickory-server migration.
- Existing `crates/` integration.

## Current Candidate Map

| Area | Current files | p2panda candidate | Initial position |
| --- | --- | --- | --- |
| Signed fact envelope and body hash | `MVP/p2panda-facts/src/lib.rs`, older wrappers in `MVP/iroh/src/facts.rs` | `p2panda-core`, `p2panda-stream` | Already adopted for new path; audit remaining iroh-docs dependency and delete/park plan. |
| Local operation storage | `MVP/p2panda-facts/src/lib.rs`, `MVP/bus/src/facts.rs`, `MVP/e2e/src/process_fact_source.rs` | `p2panda-store` memory/SQLite | Highest next substitution. Need persistent reopen and derived-index rebuild proof. |
| Projection-facing read seam | `MVP/projection/src/source.rs`, `MVP/projection/src/bus_source.rs` | Keep Ployz `FactSource`; feed from p2panda | Do not replace seam yet; shrink old sources after equivalent proofs exist. |
| Operation sync/import/export | `MVP/p2panda-facts/src/lib.rs`, E2E local operation copy | `p2panda-sync` | Candidate after persistent store proof; direct crate is more plausible than `p2panda-net`. |
| Authority membership and revocation | `MVP/bus/src/grants.rs`, `MVP/mesh/src/*.rs`, machine remove tombstone rules | `p2panda-auth` | Strong spike candidate for island membership and key revocation; not bus subject permissions. |
| Advisory lease facts | `MVP/lease/src/lib.rs`, ACME projection helpers | p2panda fact operations plus possible compaction/indexing | Store facts in p2panda; do not replace lease reducer semantics. |
| Bus request/reply, queue groups, bridge | `MVP/bus/src/*.rs` | None now; maybe copy p2panda-net supervision ideas later | Keep custom. p2panda-net broadcast/eventual-sync model does not express PloyzBus semantics. |
| Blob transfer | `MVP/iroh` / future payload work | direct `iroh-blobs`; maybe p2panda-blobs later | Do not adopt p2panda-blobs now without a compile spike. |

## Investigation Units

### Unit 1: Substrate Inventory And Deletion Ledger

Files to inspect:

- `MVP/iroh/src/facts.rs`
- `MVP/bus/src/facts.rs`
- `MVP/projection/src/bus_source.rs`
- `MVP/e2e/src/process_fact_source.rs`
- `MVP/p2panda-facts/src/lib.rs`
- `MVP/p2panda-spike/src/lib.rs`
- `MVP/e2e/src/*contract.rs`

Output:

- `MVP/design-notes/p2panda-substitution-audit.md`
- A table of each custom substrate area, exact behavior it provides, whether it
  survives, shrinks, or should be deleted after a named proof, and the test that
  protects deletion.

Key questions:

- Which older proof paths still require iroh-docs or bus facts?
- Which E2E fixtures are now only historical comparison evidence?
- Which files are product semantics versus substrate scaffolding?
- What is the smallest deletion that reduces maintenance without losing proof
  coverage?

### Unit 2: Persistent p2panda Fact Store Feasibility

Files to inspect or spike:

- `MVP/p2panda-facts/Cargo.toml`
- `MVP/p2panda-facts/src/lib.rs`
- `MVP/p2panda-spike/src/lib.rs`

Output:

- A compile-tested spike if the `p2panda-store` SQLite API is not obvious from
  docs.
- A recommendation for whether `PandaFactStore` should gain a persistent
  backend in the next implementation slice.

Proof scenarios to specify:

- write p2panda operations, close store, reopen, rebuild derived indexes from
  operations;
- import duplicate/conflict operations after reopen;
- project SQLite/gateway/DNS from rebuilt indexes;
- simulate fact-store role restart while serving/process roles keep last-good
  state.

Decision criteria:

- Adopt if the persistent backend can remove custom process fact-source
  durability and support index rebuild with a small adapter.
- Defer if the SQLite implementation requires invasive async trait plumbing
  that would obscure the current `FactSource` seam.

### Unit 3: p2panda-auth Membership Spike Plan

Files to inspect or spike:

- `MVP/bus/src/grants.rs`
- `MVP/mesh/src/domain.rs`
- `MVP/mesh/src/invite.rs`
- `MVP/machine/src/remove.rs`
- `MVP/e2e/src/membership_wireguard_contract.rs`
- `MVP/e2e/src/machine_remove_contract.rs`

Output:

- A proposed mapping from Ployz authority islands, principals, node keys,
  grants, tombstones, and re-invite behavior to `p2panda-auth` groups.
- A decision on whether a compile spike is required before ACME or machine
  remove moves further.

Proof scenarios to specify:

- root/admin can add a node principal;
- removed or demoted manager cannot keep authoring group changes;
- concurrent remove/re-add produces deterministic strong-removal behavior that
  matches or improves current tombstone semantics;
- subject/RPC grants remain Ployz-owned and are not flattened into
  `p2panda-auth`.

Decision criteria:

- Adopt for island membership if it can replace custom revocation/concurrent
  membership conflict logic without changing Ployz command semantics.
- Keep custom grants if p2panda-auth conditions cannot express subject
  wildcard/queue/temporary-response behavior clearly.

### Unit 4: p2panda-sync Versus Manual Export/Import

Files to inspect or spike:

- `MVP/p2panda-facts/src/lib.rs`
- `MVP/e2e/src/p2panda_fact_source_contract.rs`
- `MVP/e2e/src/deploy_restart_recovery_contract.rs`
- `MVP/iroh/src/facts.rs`

Output:

- A sync adoption plan or rejection note.
- An explicit statement about whether manual operation export/import remains
  acceptable for the next product slice.

Proof scenarios to specify:

- two p2panda stores exchange append-log operations through a test transport;
- offline store catches up after missed operations;
- duplicate/out-of-order operations remain idempotent;
- projection lag and import latency are recorded at 200, 1,000, and 10,000
  synthetic fact counts if the API is adopted.

Decision criteria:

- Use `p2panda-sync` directly before `p2panda-net` if it gives enough log-sync
  leverage without the iroh version mismatch.
- Keep manual import/export if the next product proof only needs deterministic
  local replication evidence.

### Unit 5: Next Product Slice Re-ranking

Inputs:

- Results from Units 1-4.
- Existing `MVP/slice-019-p2panda-acme-http01-plan.md`.

Output:

- Updated `MVP/overall-plan.md`.
- Updated or superseded `MVP/slice-019-p2panda-acme-http01-plan.md` if ACME is
  no longer the next implementation slice.

Decision choices:

1. Keep ACME next if the audit shows the current p2panda fact store is stable
   enough and the best proof is product-facing.
2. Insert persistent p2panda fact store/index-rebuild first if it unlocks
   deleting `ProcessFactSource`, proves fact-store restart, or reduces more
   future code than ACME does.
3. Insert p2panda-auth first if membership/revocation is the highest-risk area
   of custom substrate.
4. Insert p2panda-sync first only if direct sync integration is small and
   clearly replaces manual import/export.

## Test And Verification Plan

This slice is documentation and spike work, so the gate depends on what the
investigation touches.

Always run:

```text
cd MVP && cargo test -p mvp-p2panda-facts --lib
cd MVP && cargo run -p mvp-e2e -- p2panda-fact-source-contract
cd MVP && cargo run -p mvp-e2e -- deploy-restart-recovery-contract
git diff --check
```

If a persistent-store spike is added:

```text
cd MVP && cargo test -p mvp-p2panda-facts --lib
```

If an auth spike is added:

```text
cd MVP && cargo test -p mvp-p2panda-auth-spike --lib
```

If a sync spike is added:

```text
cd MVP && cargo test -p mvp-p2panda-sync-spike --lib
```

Before shipping the investigation report, run the time-budgeted all gate unless
only markdown changed:

```text
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

## Review And Simplification

- Use a simplify pass on any spike code before committing it.
- Use subagent review for the final investigation report, with at least:
  - a correctness reviewer checking that the proposed substitutions preserve
    Ployz semantics;
  - a maintainability reviewer checking whether the substitutions actually
    reduce long-term custom code;
  - a performance reviewer checking persistent-store/index/sync risks.
- Do not review tiny wording-only changes.

## Exit Criteria

The slice is complete when:

- `MVP/design-notes/p2panda-substitution-audit.md` exists and maps every major
  remaining custom substrate path to keep/replace/delete/defer;
- every recommended p2panda adoption has a named proof before product behavior
  can depend on it;
- every rejection/defer has a concrete reason, such as version mismatch,
  missing API, wrong semantics, or insufficient proof;
- `MVP/overall-plan.md` names the next implementation slice and why;
- stale ACME-next assumptions are updated if the audit changes the order;
- verification commands relevant to changed files are recorded in the report.

