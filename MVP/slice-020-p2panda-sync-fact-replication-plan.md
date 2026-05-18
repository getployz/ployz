---
title: Slice 020 p2panda Net Sync Fact Replication Plan
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
  - MVP/slice-018b-p2panda-fact-substrate-plan.md
  - MVP/slice-018c-p2panda-deploy-restart-recovery-plan.md
  - MVP/slice-019b-persistent-p2panda-fact-store-plan.md
external:
  - https://docs.rs/p2panda-sync/latest/p2panda_sync/
  - https://docs.rs/p2panda-sync/latest/p2panda_sync/protocols/log_sync/index.html
  - https://docs.rs/p2panda-stream/latest/p2panda_stream/
  - https://docs.rs/p2panda-net/latest/p2panda_net/
---

# Slice 020 p2panda Net Sync Fact Replication Plan

## Problem Frame

Slice 019b made p2panda operations durable in SQLite and made Ployz's derived
indexes rebuildable. The next false boundary is replication: current E2E
proofs exchange p2panda operations by iterating `export_operations` and calling
`import_operation` directly. That is acceptable deterministic harness plumbing,
but it should not become the shape ACME or later product commands rely on.

This slice should prove the generic fact-replication substrate before the next
product canary:

```text
two persistent p2panda fact stores
  -> run real p2panda-net endpoint/gossip/log-sync replication where workable
  -> import received operations through Ployz authorization/trust checks
  -> rebuild projection SQLite and snapshots from the synced store
  -> preserve duplicate, conflict, and payload-read semantics
```

This is allowed to be production-shaped transport. Initial crate probes show
`p2panda-net` from crates.io is not usable today, but git `main` compiles when
`p2panda-store/sqlite` is enabled explicitly. Bias toward using `p2panda-net`
for iroh endpoint, gossip, address book, and log sync in this slice. Fall back
to lower-level `p2panda-sync` only if `p2panda-net` prevents Ployz-owned
authorization, candidate-status reporting, or scale metrics.

## Requirements Trace

- `VISION.md`: durable state records explicit lifecycle facts; background work
  must not silently rewrite cluster truth; stale-state-served-silently is the
  worst failure class.
- `MVP/overall-plan.md`: facts write durably on the connected node and
  replicate eventually; each slice must stop deferring p2panda persistence/sync
  and iroh connectivity decisions when it touches facts/transport.
- `MVP/architecture.md`: durable facts are signed p2panda operation logs behind
  `FactSource`; projection and SQLite remain disposable local state.
- `MVP/e2e-proof-plan.md`: the fact gate needs eventual replication,
  conflict-candidate preservation, projection rebuild, and propagation metrics
  beyond single local-store proofs.
- `MVP/primitive-decisions.md`: manual p2panda export/import is intentionally
  narrow and not a production sync protocol.
- `MVP/design-notes/p2panda-substitution-audit.md`: persistent stores now
  exist, and follow-up probes changed the net decision from "defer" to "try
  `p2panda-net` early, with a measured fallback to lower-level `p2panda-sync`."
- `MVP/slice-019b-persistent-p2panda-fact-store-plan.md`: p2panda operation
  logs are durable truth; derived Ployz indexes and projection SQLite are
  disposable.

## Scope

In scope:

- Add git `p2panda-net` to the isolated `MVP/` workspace if it can be pinned
  cleanly, plus explicit git `p2panda-store` with `sqlite` enabled.
- Use `p2panda-net` `Endpoint`, `Gossip`, and `LogSync` for the first
  production-shaped fact replication proof where its API fits the existing
  Ployz fact store.
- Add a narrow Ployz adapter inside `mvp-p2panda-facts` that maps p2panda net
  sync events back through existing Ployz authorization/trust checks.
- Sync between two persistent SQLite-backed `PandaFactStore` instances through
  local iroh endpoints where practical. If the API blocks the existing store
  adapter, run a lower-level `p2panda-sync` proof in the same slice and leave
  the `p2panda-net` compile/API blocker documented with the seam that failed.
- Keep Ployz-owned trust, island, principal, grants, candidate status, and
  payload-read checks in `mvp-p2panda-facts`.
- Keep `FactSource` synchronous and projection-facing.
- Preserve current manual `export_operations` / `import_operation` as
  deterministic test/debug plumbing until enough scenarios no longer need it.
- Add `p2panda-sync-fact-source-contract` to `mvp-e2e`.
- Record sync metrics: operation counts, bytes, sync duration, repeated-sync
  no-op duration, projection rebuild duration, and large-load timings.

Out of scope:

- ACME behavior. It is the next product canary after this slice.
- `p2panda-auth` membership and strong-removal semantics.
- p2panda blobs.
- Replacing PloyzBus request/reply. p2panda gossip/sync can carry fact
  replication and wakeups; it does not replace NATS-shaped request/reply,
  queue groups, no-responders, service imports/exports, or subject grants.
- Replacing PloyzBus.
- Changing reducers to async.
- Deleting `IrohDocsFactSource`, `BusFactSource`, `ProcessFactSource`, or
  `MVP/p2panda-spike`.
- Existing `crates/` or root workspace integration.

## Crate Scout

Checked for this plan:

- `p2panda-sync` 0.5.2 is available and matches the p2panda crate line already
  used by `mvp-p2panda-facts`. Its public docs describe data-type agnostic
  sync over `Sink` / `Stream` pairs, plus concrete append-log sync protocols.
- `p2panda_sync::protocols::LogSync` syncs append-only logs by exchanging
  local log heights, calculating deltas, sending missing operations, and then
  repeating the exchange in the reverse direction. That matches Ployz's current
  signed fact operation model.
- `p2panda-sync::protocols::TopicLogSync` adds topic mapping and optional
  live-mode. It is useful later, but `LogSync` is a smaller first proof because
  Ployz already has island/topic selection and can construct the exact log map
  from trusted author keys.
- `p2panda-sync` emits received operations to the application layer; it does
  not automatically apply Ployz authorization semantics. That is correct:
  received operations must still flow through `PandaFactStore` import
  validation and derived-index rebuild/update.
- `LogSync` emits `LogSyncEvent::Data` through its event channel rather than
  returning received operations from the protocol run call. The Ployz adapter
  must own that event receiver and drain it while the protocol runs.
- `p2panda-net` crates.io 0.5.2 is not usable as a straight dependency today:
  default features fail in the old `ed25519-dalek` prerelease stack, and
  `default-features = false, features = ["supervisor"]` still compiles modules
  that reference disabled `iroh` and `p2panda-discovery` dependencies.
- `p2panda-net` git `main` does compile in isolation when the consumer also
  depends on git `p2panda-store` with `features = ["sqlite"]`. This is the
  preferred workaround for the MVP because it lets us test their iroh/gossip/
  log-sync stack before writing more of our own.
- `p2panda-net` exposes `AddressBook`, `Endpoint`, `Gossip`, and `LogSync`.
  Its sync tests show local iroh endpoints exchanging p2panda operations and
  then switching into live mode. That is closer to the desired MVP substrate
  than a bespoke in-memory sync stream.
- `p2panda-stream` remains the ingestion/validation layer for storing received
  operations. Do not bypass the current import path's signature, body-hash,
  author-key, and grant checks.

Decision:

- Adopt git `p2panda-net` first if a minimal two-node proof can preserve Ployz
  import authorization and metrics.
- Pin the git revision in `MVP/Cargo.toml`; do not track an unpinned branch.
- Keep lower-level `p2panda-sync` as the fallback and escape hatch, not the
  preferred path.
- Add only the API needed to run a bounded two-party sync session for known
  islands and trusted authors.
- Do not expose p2panda network types to deploy, ACME, machine, serving, or
  projection reducers.

## Design Decisions

### Sync Is Transport Plumbing, Not Authority

Sync decides which p2panda append-log operations are missing. It does not
decide whether a Ployz principal may write a fact, whether a payload is readable
by a projection session, or which conflict candidate wins.

The import path remains authoritative:

```text
sync receives operation
  -> p2panda header/body decode
  -> p2panda operation validation
  -> trusted author-key binding check
  -> Ployz island/principal/fact-write authorization
  -> derived index update
```

If the sync protocol transfers an operation that Ployz does not trust, the
sync session should report an import failure with structured context. Do not
silently add it as verified truth.

### The Adapter Owns The Event Import Bridge

`p2panda-sync` should not leak protocol event handling into business code or
E2E scenarios. The adapter must create the event channel, subscribe before
running `LogSync`, drain `LogSyncEvent::Data` while the protocol is active, and
convert each received header/body pair into the existing `PandaFactOperation`
import path.

The sync report should be built from that same bridge:

```text
LogSyncEvent::Data
  -> PandaFactOperation
  -> PandaFactStore::import_operation
  -> inserted / duplicate / conflict / structured failure counts
```

Status/completion events from `LogSync` should feed metrics and error context,
not bypass the Ployz import checks.

### Trusted Author Logs Define The Sync Scope

`p2panda-sync` needs the set of logs to compare. Ployz should derive that set
from explicit trusted author-key bindings for the target island, not by
discovering random authors inside the store.

For this slice:

```text
PandaFactSyncScope {
  island: IslandId,
  trusted_authors: BTreeMap<PrincipalId, PandaFactAuthorKey>,
}
```

Open question deferred to future membership work: how p2panda-auth or island
membership facts populate this trusted-author set. This slice takes it from
explicit config/test setup.

### Live Mode Is Allowed If p2panda-net Gives It For Free

The first proof must still be able to assert one-shot catch-up and no-op
resync. If `p2panda-net::LogSync` naturally enters live mode after initial
sync, keep it and prove one live fact delivery as an extra assertion. Do not
write our own live-loop machinery in this slice.

Defer custom live-mode policy until:

- one-shot sync is proven with persistent stores,
- p2panda-net proves the basic endpoint/gossip/log-sync path,
- process roles need long-lived sync sessions,
- operator-visible sync status exists.

### Keep Manual Export/Import Named As Harness Plumbing

Existing manual exchange remains useful for deterministic tests and targeted
failure injection. Do not delete it in this slice. Do add documentation and
names that prevent future product code from mistaking it for the production
replication path.

### Do Not Add ACME Yet

ACME remains the next product canary. This slice exists so that ACME can prove
advisory lease and HTTP-01 business semantics over a real p2panda sync
boundary, not over manual operation copying.

## Implementation Units

### Unit 0: p2panda-net Workaround Probe

Files:

- `MVP/Cargo.toml`
- `MVP/p2panda-facts/Cargo.toml`
- `MVP/p2panda-facts/src/lib.rs`

Work:

- Add git dependencies for `p2panda-net`, `p2panda-store` with `sqlite`,
  `p2panda-sync`, and matching p2panda crates from one pinned git revision.
- Prove the dependency graph inside the MVP workspace with a minimal compile
  test before writing adapter code.
- Instantiate two local p2panda net nodes with address books, iroh endpoints,
  gossip, and `LogSync`.
- Verify the proof can subscribe to sync events and observe received
  operations, sync completion, and live operation delivery.
- If this fails because of API shape rather than dependency resolution, capture
  the exact blocker in this plan/report and use Unit 1 fallback. If it works,
  Unit 1 should be p2panda-net-backed.

Verification:

- `cd MVP && cargo check -p mvp-p2panda-facts`

### Unit 1: p2panda Net Fact Adapter Surface

Files:

- `MVP/p2panda-facts/Cargo.toml`
- `MVP/p2panda-facts/src/lib.rs`

Work:

- Introduce small public sync types:
  - `PandaFactSyncScope`,
  - `PandaFactSyncReport`,
  - `PandaFactSyncError`,
  - a net-backed sync runner or session helper.
- Map island facts into a p2panda `Topic` and associated append logs.
- Create and subscribe to the `LogSync` event channel before starting the
  protocol so received operations are not dropped and the protocol does not
  fail on missing receivers.
- Run two peer sessions through `p2panda-net::LogSync` where possible.
- Concurrently drain `LogSyncEvent::Data`, convert received p2panda operations
  into `PandaFactOperation`, and import them through the existing validation
  path.
- Import every received operation through the existing `PandaFactStore`
  validation path.
- Make sync idempotency observable in the report: received, imported,
  duplicate, conflict, and failed counts.
- Keep received-operation import errors structured; do not collapse them into
  strings except at display boundaries.
- Do not expose p2panda store internals outside `mvp-p2panda-facts`.

Tests:

- Empty stores sync successfully with zero imported operations.
- One-way catch-up from store A to store B imports every missing operation.
- Re-running sync after convergence reports duplicates/no-ops without adding
  candidates.
- Bidirectional sync exchanges operations written independently on both sides.
- Same-key different-payload operations survive sync as conflict candidates.
- A received operation signed by an untrusted key fails import instead of
  becoming verified truth. This should be an adversarial/asymmetric protocol
  test because a normal trusted-author scope will not request random unknown
  author logs.
- A received operation from a trusted author without a Ployz fact-write grant
  fails import with `UnauthorizedWrite`.

Verification:

- `cd MVP && cargo test -p mvp-p2panda-facts --lib`
- `cd MVP && cargo clippy -p mvp-p2panda-facts --all-targets -- -D warnings`

### Unit 2: p2panda Sync E2E Fact Source Contract

Files:

- `MVP/e2e/Cargo.toml`
- `MVP/e2e/src/p2panda_sync_fact_source_contract.rs`
- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/projection_harness.rs`
- `MVP/e2e/src/p2panda_fact_source_contract.rs`

Work:

- Add `p2panda-sync-fact-source-contract`.
- Create two persistent SQLite-backed `PandaFactStore` instances with explicit
  trusted author keys and separate projection sessions.
- Write node/service/serving/DNS facts to store A while store B is empty.
- Run the p2panda net sync adapter from A to B.
- Project from store B, write gateway/DNS snapshots, and verify the projection
  matches store A's source facts.
- Delete B's projection SQLite and rebuild from synced p2panda operations.
- Re-run sync and assert no duplicate projection candidates are introduced.
- Add a same-key/different-payload race and prove both stores converge to the
  same conflict-candidate set after bidirectional sync.
- Keep this scenario independent from `docs_backed_acme_http01_contract`; ACME
  comes later.

Required assertions:

- exact projected node/service/gateway/DNS counts,
- exact no-op sync duplicate count after convergence,
- conflict candidate count after bidirectional sync,
- no cross-island leakage,
- deleted projection SQLite rebuilds from synced store B,
- payload reads still require exact candidate identity and projection read
  grants,
- sync reports received/imported/duplicate/conflict counts,
- if live mode is enabled, one post-convergence fact written on A arrives at B
  without a manual export/import loop.

Metrics:

- first sync duration,
- repeated no-op sync duration,
- bidirectional conflict sync duration,
- projection rebuild duration,
- synced operation count,
- synced byte count,
- gateway and DNS snapshot byte sizes.

Verification:

- `cd MVP && cargo run -p mvp-e2e -- p2panda-sync-fact-source-contract`
- `cd MVP && cargo run -p mvp-e2e -- p2panda-fact-source-contract`

### Unit 3: Large-Load Sync Probe

Files:

- `MVP/e2e/src/p2panda_sync_fact_source_contract.rs`
- `MVP/e2e/src/scale.rs`
- `MVP/e2e/src/main.rs`

Work:

- Add a bounded large-load proof for p2panda sync. Prefer keeping it inside the
  new scenario first; move shared reporting into `scale.rs` only if needed.
- Use product-relevant sizes plus one stress size:
  - 200 logical facts,
  - 1,000 logical facts,
  - 10,000 logical facts.
- Record sync duration, projection duration, imported operation counts,
  duplicate/no-op counts, and memory snapshots when cheap.
- Fail on exact convergence failures; use the existing `MVP_E2E_ALL_TIMEOUT`
  budget for wall-clock protection.
- Do not add sleeps to stabilize ordering. If the protocol ordering is wrong,
  fix the sync/import boundary.

Verification:

- `cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all`

### Unit 4: Maintainer Docs And Next Product Boundary

Files:

- `MVP/primitive-decisions.md`
- `MVP/e2e-proof-plan.md`
- `MVP/overall-plan.md`
- `MVP/design-notes/p2panda-substitution-audit.md`
- `MVP/slice-021-p2panda-acme-http01-plan.md` or the parked ACME plan if it is
  renamed in this slice

Work:

- Add a "Changed Since Last Slice" entry for Slice 019b if still missing:
  persistent p2panda SQLite storage, rebuildable indexes, p2panda-fed
  process-role serving proof, and `BeginRebuild` as the explicit source refresh
  boundary.
- Add a Slice 020 entry explaining the p2panda-net workaround/adoption and why
  manual export/import remains harness plumbing.
- Update `MVP/e2e-proof-plan.md` with the new sync scenario and metrics.
- Update `MVP/overall-plan.md` so the next product proof after this slice is
  clearly ACME on p2panda net/sync, not another substrate detour.
- If the parked ACME plan remains accurate, rename or retitle it as Slice 021
  and adjust its preconditions to depend on this sync slice.
- Record semantic-leverage expectations: this slice is substrate leverage; the
  next ACME slice must pay it off by adding product behavior without more
  replication scaffolding.

Verification:

- `git diff --check -- MVP/primitive-decisions.md MVP/e2e-proof-plan.md MVP/overall-plan.md MVP/design-notes/p2panda-substitution-audit.md MVP/slice-020-p2panda-sync-fact-replication-plan.md MVP/slice-021-p2panda-acme-http01-plan.md`

## Verification

Before pushing the slice:

```text
cd MVP && cargo fmt --all
cd MVP && cargo test -p mvp-p2panda-facts --lib
cd MVP && cargo test -p mvp-projection --lib
cd MVP && cargo test -p mvp-e2e
cd MVP && cargo clippy -p mvp-p2panda-facts -p mvp-e2e --all-targets -- -D warnings
cd MVP && cargo run -p mvp-e2e -- p2panda-sync-fact-source-contract
cd MVP && cargo run -p mvp-e2e -- p2panda-fact-source-contract
cd MVP && cargo run -p mvp-e2e -- p2panda-process-role-serving-contract
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

If `mvp-e2e -- all` exceeds the 120-second budget, diagnose the regression
instead of raising the budget by default.

## Review Focus

Use review subagents for the implementation slice after the first green proof.
Do not run a review workflow for tiny mechanical fixes inside the slice.

Focus areas:

- correctness: missing-log calculation, bidirectional catch-up, duplicate
  idempotency, conflict-candidate preservation, and exact payload identity,
- authorization/security: trusted author-key scope, fact-write grants on
  import, island isolation, and no promotion of unknown authors,
- reliability: interrupted or failed sync preserves existing derived state and
  reports an operator-visible failure,
- performance: no O(N) source reopen on hot projection paths, bounded large
  load, no hidden sleeps,
- simplicity: sync/net plumbing is isolated in `mvp-p2panda-facts`; reducers,
  ACME, deploy, machine, and serving do not learn p2panda-net or p2panda-sync
  types.

Run the simplify workflow after the first green E2E proof and land that pass as
a separate commit.

## Acceptance Gate

The slice is complete when:

- `p2panda-sync-fact-source-contract` passes,
- two persistent p2panda SQLite stores converge through `p2panda-net::LogSync`
  if workable, otherwise through lower-level `p2panda-sync` with the net
  blocker documented,
- repeated sync is idempotent and observable as no-op/duplicate work,
- bidirectional same-key conflicts remain conflict candidates for reducers,
- a synced store can rebuild deleted projection SQLite and gateway/DNS
  snapshots,
- untrusted or unauthorized received operations do not become verified truth,
- large-load sync reports exact convergence at 200, 1,000, and 10,000 facts
  within the existing E2E all timeout,
- manual export/import is still available only as named harness/debug plumbing,
- maintainer docs state that ACME is next and should use this sync boundary.
