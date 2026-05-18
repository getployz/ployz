---
title: Slice 020 p2panda Sync Fact Replication Plan
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

# Slice 020 p2panda Sync Fact Replication Plan

## Problem Frame

Slice 019b made p2panda operations durable in SQLite and made Ployz's derived
indexes rebuildable. The next false boundary is replication: current E2E
proofs exchange p2panda operations by iterating `export_operations` and calling
`import_operation` directly. That is acceptable deterministic harness plumbing,
but it should not become the shape ACME or later product commands rely on.

This slice proves the fact-replication boundary before the next product canary:

```text
two persistent p2panda fact stores
  -> run p2panda append-log sync over an in-memory stream
  -> import received operations through Ployz authorization/trust checks
  -> rebuild projection SQLite and snapshots from the synced store
  -> preserve duplicate, conflict, and payload-read semantics
```

This is now a net-first slice. Earlier planning tried to prove only
`p2panda-sync` over in-memory streams, but that kept pushing the real
connectivity question forward. The slice should first prove that the real MVP
workspace can compile and spawn git `p2panda-net` log-sync nodes, then replace
manual p2panda operation copying through the smallest p2panda-backed network
surface we can make reliable.

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
- `MVP/design-notes/p2panda-substitution-audit.md`: originally advised
  evaluating lower-level `p2panda-sync` first. The operator overrode that
  posture: bias toward `p2panda-net` because its maintained network stack is
  more likely to be correct than bespoke MVP transport code.
- `MVP/slice-019b-persistent-p2panda-fact-store-plan.md`: p2panda operation
  logs are durable truth; derived Ployz indexes and projection SQLite are
  disposable.

## Scope

In scope:

- Add `p2panda-sync` to the isolated `MVP/` workspace.
- Add a git `p2panda-net` compatibility proof to the isolated `MVP/`
  workspace and align the local iroh family as needed.
- Add a narrow p2panda-sync adapter inside `mvp-p2panda-facts`.
- Sync between two persistent SQLite-backed `PandaFactStore` instances over an
  in-memory typed stream.
- Keep Ployz-owned trust, island, principal, grants, candidate status, and
  payload-read checks in `mvp-p2panda-facts`.
- Treat same-island fact sync peers as trusted replicas for this slice: sync may
  transfer payload bytes only to peers authorized to replicate the island, not
  to arbitrary projection readers.
- Keep `FactSource` synchronous and projection-facing.
- Preserve current manual `export_operations` / `import_operation` as
  deterministic test/debug plumbing until enough scenarios no longer need it.
- Add `p2panda-sync-fact-source-contract` to `mvp-e2e`.
- Record sync metrics: operation counts, bytes, sync duration, repeated-sync
  no-op duration, projection rebuild duration, and large-load timings.

Out of scope:

- ACME behavior. It is the next product canary after this slice.
- `p2panda-auth` membership and strong-removal semantics.
- Broad p2panda discovery, blobs, address-book, and production deployment
  topology work beyond the minimum needed to prove local log-sync nodes.
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
- `p2panda_sync::protocols::LogSync` syncs append-only logs by exchanging local
  log heights, calculating deltas, sending missing operations, and then
  repeating the exchange in the reverse direction. That matches Ployz's current
  signed fact operation model.
- `p2panda-sync::protocols::TopicLogSync` adds topic mapping and optional
  live-mode. It is useful later, but `LogSync` is a smaller first proof because
  Ployz already has island/topic selection and can construct the exact log map
  from trusted author keys.
- `LogSync` emits `LogSyncEvent::Data` through its event channel rather than
  returning received operations from the protocol run call. The Ployz adapter
  must own that event receiver and drain it while the protocol runs.
- `p2panda-sync` does not automatically apply Ployz authorization semantics.
  That is correct: received operations must still flow through
  `PandaFactStore` import validation and derived-index rebuild/update.
- `p2panda-net` includes iroh endpoint/discovery/gossip/log-sync modules. It
  currently fits git p2panda APIs more cleanly than the crates.io 0.5.2 network
  package, so this slice pins the git revision in dev/test first and keeps the
  production fact store on stable p2panda crates until a deliberate API
  migration.
- `p2panda-stream` remains the ingestion/validation layer for storing received
  operations. Do not bypass the current import path's signature, body-hash,
  author-key, and grant checks.

Decision:

- Adopt p2panda-maintained sync/network plumbing now, behind
  `mvp-p2panda-facts`.
- Start with a git `p2panda-net` spawn/log-sync smoke test before adding a
  broader adapter. This proves dependency compatibility in the real workspace
  and avoids writing more bespoke transport while the p2panda network stack is
  usable.
- Add only the API needed to run a bounded two-party sync session for known
  islands and trusted replica peers.
- Keep transport abstract: the adapter should operate on a generic typed sink
  and stream so a later iroh stream codec can wrap it without rewriting fact
  semantics.

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

### Sync Egress Is Replica Authorization

Projection read grants answer "may this principal read this payload through a
query surface?" Sync egress answers a different question: "may this peer
receive raw operation bodies as a replica?" Import/read checks are not enough
because payload bytes have already crossed the transport by then.

For this slice, only same-island trusted replica peers are allowed to run fact
sync and receive payload bytes. That is intentionally narrower than exposing
sync to arbitrary principals. Later cross-island or partial-replica sync needs
a separate export/read-filter design before payloads leave the sender.

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
from store-owned trusted author-key bindings for the target island, not by
discovering random authors inside the store or trusting caller-provided key
mappings.

For this slice:

```text
PandaFactSyncScope {
  island: IslandId,
  trusted_authors: BTreeMap<PrincipalId, PandaFactAuthorKey>,
}
```

`PandaFactSyncScope` is selection-only. Before running sync, every entry must
match the store's canonical `(island, principal) -> author key` binding. Import
validation must never consult the sync scope as its source of truth.

Open question deferred to future membership work: how p2panda-auth or island
membership facts populate the store-owned trusted-author set. This slice takes
it from explicit config/test setup, then validates the requested scope against
the store before syncing.

### No Live Mode Yet

The first proof should be one-shot catch-up. Live mode is attractive but would
mix protocol proof, session lifecycle, cancellation, and event forwarding.

Defer live mode until:

- one-shot sync is proven with persistent stores,
- p2panda-sync messages have an iroh stream transport,
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

### Unit 0: p2panda-net Compatibility Proof

Files:

- `MVP/Cargo.lock`
- `MVP/e2e/Cargo.toml`
- `MVP/iroh/Cargo.toml`
- `MVP/p2panda-facts/Cargo.toml`
- `MVP/p2panda-facts/src/lib.rs`
- `MVP/serving/Cargo.toml`

Work:

- Align the isolated `MVP/` iroh family to the line required by git
  `p2panda-net`; using non-rc iroh is acceptable for this MVP workspace.
- Keep production `mvp-p2panda-facts` on stable crates.io
  `p2panda-core/store/stream 0.5.2` for this unit.
- Add git `p2panda-net` and matching git p2panda core/store crates as
  dev/test-only dependencies.
- Add a smoke test that spawns two local `p2panda-net` test nodes and starts a
  log-sync stream for the same topic.
- Do not add a half-custom lower-level sync adapter in this unit; that belongs
  after the net stack dependency surface is proven.

Verification:

- `cd MVP && cargo test -p mvp-p2panda-facts p2panda_net_git_stack_spawns_local_log_sync_nodes -- --nocapture`
- `cd MVP && cargo test -p mvp-p2panda-facts --lib`
- `cd MVP && cargo check -p mvp-serving`
- `cd MVP && cargo check -p mvp-iroh`

### Unit 1: p2panda-sync Adapter Surface

Files:

- `MVP/p2panda-facts/Cargo.toml`
- `MVP/p2panda-facts/src/lib.rs`

Work:

- Add `p2panda-sync = "0.5.2"`.
- Introduce small public sync types:
  - `PandaFactSyncScope`,
  - `PandaFactSyncReport`,
  - `PandaFactSyncError`,
  - a one-shot sync runner or session helper.
- Build the `LogSync` log map from a requested scope only after checking the
  scope against store-owned trusted-author bindings.
- Bind each sync session to a same-island trusted replica principal. Do not let
  ordinary projection readers or arbitrary peers receive raw operation bodies.
- Create and subscribe to the `LogSync` event channel before starting the
  protocol so received operations are not dropped and the protocol does not
  fail on missing receivers.
- Run two peer sessions over generic typed sinks/streams, or expose a lower
  helper that an E2E harness can wire with paired in-memory channels.
- Concurrently drain `LogSyncEvent::Data`, convert received p2panda operations
  into `PandaFactOperation`, and import them through the existing validation
  path.
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
- Malicious sync scope key substitution is rejected before sync starts.
- A principal with projection read permission but without replica-sync authority
  cannot start sync or receive operation payload bytes.

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
  trusted author keys, replica-sync authority, and separate projection sessions.
- Write node/service/serving/DNS facts to store A while store B is empty.
- Run the p2panda sync adapter from A to B.
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
- sync reports received/imported/duplicate/conflict counts.

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
- `MVP/slice-021-p2panda-acme-http01-plan.md`

Work:

- Add a "Changed Since Last Slice" entry for Slice 019b if still missing:
  persistent p2panda SQLite storage, rebuildable indexes, p2panda-fed
  process-role serving proof, and `BeginRebuild` as the explicit source refresh
  boundary.
- Add a Slice 020 entry explaining the p2panda-sync adoption and why manual
  export/import remains harness plumbing.
- Update `MVP/e2e-proof-plan.md` with the new sync scenario and metrics.
- Update `MVP/overall-plan.md` so the next product proof after this slice is
  clearly ACME on p2panda sync, not another substrate detour.
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
- authorization/security: trusted author-key scope, replica-sync authority,
  fact-write grants on import, island isolation, and no promotion of unknown
  authors,
- reliability: interrupted or failed sync preserves existing derived state and
  reports an operator-visible failure,
- performance: no O(N) source reopen on hot projection paths, bounded large
  load, no hidden sleeps,
- simplicity: sync plumbing is isolated in `mvp-p2panda-facts`; reducers,
  ACME, deploy, machine, and serving do not learn p2panda-sync types.

Run the simplify workflow after the first green E2E proof and land that pass as
a separate commit.

## Acceptance Gate

The slice is complete when:

- `p2panda-sync-fact-source-contract` passes,
- two persistent p2panda SQLite stores converge through `p2panda-sync`, not
  manual operation copying,
- sync egress is limited to same-island trusted replica peers,
- repeated sync is idempotent and observable as no-op/duplicate work,
- bidirectional same-key conflicts remain conflict candidates for reducers,
- a synced store can rebuild deleted projection SQLite and gateway/DNS
  snapshots,
- untrusted or unauthorized received operations do not become verified truth,
- large-load sync reports exact convergence at 200, 1,000, and 10,000 facts
  within the existing E2E all timeout,
- manual export/import is still available only as named harness/debug plumbing,
- maintainer docs state that ACME is next and should use this sync boundary.
