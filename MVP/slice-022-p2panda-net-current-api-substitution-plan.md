---
title: Slice 022 p2panda-net Current API Substitution Plan
status: completed
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution.md
  - MVP/design-notes/p2panda-substitution-audit.md
  - MVP/slice-020-p2panda-sync-fact-replication-plan.md
  - MVP/slice-021-p2panda-acme-http01-plan.md
  - MVP/slice-021-p2panda-acme-http01.md
external:
  - https://docs.rs/p2panda-net/latest/p2panda_net/
  - https://docs.rs/p2panda-sync/latest/p2panda_sync/
  - https://docs.rs/p2panda-store/latest/p2panda_store/
  - https://github.com/p2panda/p2panda
---

# Slice 022 p2panda-net Current API Substitution Plan

## Problem Frame

Slices 020 and 021 proved that deploy recovery and ACME can run over signed
p2panda facts, p2panda-sync replication, rebuildable SQLite projections, and
last-good serving state. That is the right semantic direction, but the substrate
still carries a split shape:

```text
production mvp-p2panda-facts
  -> crates.io p2panda-core/store/stream/sync 0.5.2
  -> custom in-process LogSync pair

dev/test compatibility proof
  -> git p2panda-net
  -> git p2panda-core/store API line
  -> local iroh endpoint + gossip + log-sync nodes
```

This slice answers whether the current p2panda stack can delete or shrink
Ployz-owned substrate code now, without weakening the Ployz authority boundary.

The success condition is not "use p2panda-net somewhere." The success condition
is one of these two honest outcomes:

1. safely substitute current p2panda/p2panda-net APIs for a meaningful part of
   the custom MVP sync/transport plumbing, with E2E proof and a LOC/shape
   reduction; or
2. document the exact blocker and keep the current adapter intentionally, with
   a narrow p2panda-net proof showing what remains before migration.

Do not ship a larger substrate just to say the slice used p2panda-net.

## Requirements Trace

- `VISION.md`: the operator's connected node is the consistency boundary;
  durable facts replicate eventually; steady state must keep working when the
  coordinator role is absent.
- `MVP/overall-plan.md`: after Slice 021, the next proof should find the
  largest safe deletion of Ployz-owned substrate code using p2panda-net/current
  APIs where they reduce maintenance burden.
- `MVP/architecture.md`: p2panda signed operation logs sit behind
  `FactSource`; Ployz owns authority, reducers, and business semantics.
- `MVP/e2e-proof-plan.md`: E2E-4 still needs real transport-bound p2panda sync,
  and E2E-9 needs semantic leverage evidence instead of only more primitives.
- `MVP/primitive-decisions.md`: Slice 020 intentionally kept git p2panda-net as
  dev/test until a separate migration evaluates the current API line.
- `MVP/slice-021-p2panda-acme-http01.md`: ACME now uses the generic p2panda
  sync boundary. The next slice should bias toward deleting Ployz substrate
  code, not adding ACME-specific replication.

## Scope

In scope:

- Compare the current git p2panda API line against the crates.io 0.5.2 API line
  used by `mvp-p2panda-facts`.
- Try a production dependency substitution only if it preserves Ployz import
  validation, trusted author bindings, same-island replica checks, conflict
  candidates, and payload-read authorization.
- Add a transport-bound p2panda-net proof using local iroh-backed nodes if the
  current APIs can feed received operations through the canonical Ployz import
  path.
- Add an E2E scenario named `p2panda-net-sync-contract` or, if substitution is
  blocked, a narrower `p2panda-net-api-substitution-contract` that records the
  blocker as executable evidence.
- Keep all work inside `MVP/`.
- Preserve `sync_panda_fact_stores` as deterministic harness/debug plumbing
  unless the new proof fully replaces its existing product-proof use cases.
- Record LOC/maintenance-burden impact in the slice report and
  `MVP/e2e-proof-plan.md`.
- Keep `cargo run -p mvp-e2e -- all` time-budgeted.

Out of scope:

- Replacing PloyzBus.
- Adding p2panda-auth membership, p2panda-blobs payload storage, or encrypted
  p2panda spaces.
- Direct raw-iroh transport work if p2panda-net provides the maintained
  transport/sync path.
- Real multi-host deployment, relay configuration, or production discovery.
- Any change outside `MVP/`.
- Quorum, witness acks, hidden active-partition checks, or strict leases.
- Rewriting ACME or deploy business logic.

## Crate Scout

Checked before planning:

- `p2panda-net` docs describe data-type-agnostic peer-to-peer networking,
  discovery, gossip, local-first log sync, iroh endpoints, address books, and
  supervisor support. The docs also state the APIs are still under active
  development and not yet stable for production use.
- `p2panda-net::LogSync` on the pinned git revision builds around the current
  p2panda `LogStore`, `TopicStore`, `Operation<E>`, `Topic`, and `VerifyingKey`
  types. Its `stream(topic, live_mode)` returns a `SyncHandle`; tests show
  `OperationReceived`, `SyncFinished`, `LiveModeStarted`, and live
  `publish(...)` events.
- `p2panda-sync` docs say high-level users should usually enter through
  `p2panda-net`; lower-level `p2panda-sync` is for custom protocols or custom
  manager integration.
- `p2panda-store` 0.5.2 has the crates.io SQLite store API currently used by
  `mvp-p2panda-facts`. The git store API has moved to
  `SqliteStoreBuilder`, `logs::LogStore`, `operations::OperationStore`, and
  `topics::TopicStore` shapes.
- Slice 020 already proved the pinned git `p2panda-net` stack can compile and
  spawn local log-sync nodes in the `MVP/` workspace.

Decision:

- Try the current p2panda API substitution before adding another hand-rolled
  transport wrapper.
- Treat API instability as a migration risk, not an automatic blocker. The gate
  is whether the current line reduces Ployz-maintained code while preserving
  validation and proof coverage.
- If p2panda-net stores incoming operations before Ployz validates them, do not
  treat that store as canonical Ployz truth. Use a quarantine/import bridge or
  stop the substitution.

## Design Decisions

### Authority Still Lives Above p2panda-net

p2panda-net can discover peers, open iroh-backed sessions, gossip, and sync
append logs. It must not decide that a remote operation is trusted Ployz truth.

The canonical path remains:

```text
received p2panda operation
  -> decode/validate p2panda operation
  -> same-island session check
  -> trusted author-key binding
  -> Ployz writer grant check
  -> conflict-as-candidate indexing
  -> projection reads through FactSource
```

If p2panda-net's default `LogSync` inserts into its own store before Ployz can
authorize the operation, that store is a transport/quarantine store for this
slice, not the canonical `PandaFactStore`.

### Substitution Needs A Deletion Or A No-Go

This slice should not leave the MVP with both a new p2panda-net adapter and all
old custom paths still promoted as product surfaces.

Acceptable outcomes:

- Production `mvp-p2panda-facts` moves to the current p2panda API line and
  deletes compatibility aliases or duplicated sync glue.
- p2panda-net replaces a meaningful part of custom in-process sync for an E2E
  proof while preserving the Ployz import path.
- The slice proves the exact API/authority reason substitution is unsafe today,
  keeps the current Slice 020 adapter, and documents the next concrete upstream
  or local seam needed.

Unacceptable outcome:

- A larger second sync stack where business code can accidentally choose the
  wrong authority path.

### One-Shot Catch-Up Before Live Mode

p2panda-net live mode is attractive, but live sessions add lifecycle,
cancellation, backpressure, and outage status. The first production-shaped
proof should be bounded catch-up over local iroh-backed nodes.

Live mode may be included only if it falls out naturally from the current API
and the E2E can prove clean shutdown, duplicate no-op import, and no stale
serving rollback. Otherwise it stays deferred.

### Time Budget Is Part Of The Contract

Slice 021 left `cargo run -p mvp-e2e -- all` under a 120s budget. This slice
must keep the all-run budgeted and fail the slice if p2panda-net proof work
turns the default E2E suite into an unbounded network wait.

Use explicit deadlines around network/session waits. No external control-plane
I/O may await indefinitely.

### Maintenance Burden Is A Measured Output

The current LOC picture is mixed:

```text
deploy core: strong MVP reduction
serving shape: strong MVP reduction with production-adapter caveats
ACME: moderate shape win
bus/fact/projection substrate: current cost center
```

This slice should update that picture with actual numbers. A p2panda-net move
that adds more substrate without deleting or simplifying a Ployz path is a
negative result.

## Implementation Units

### Unit 1: Current p2panda API Characterization

Files:

- `MVP/p2panda-facts/Cargo.toml`
- `MVP/p2panda-facts/src/lib.rs`
- `MVP/slice-022-p2panda-net-current-api-substitution-plan.md`

Work:

- Build a small characterization around the pinned git p2panda store/core API:
  author operation creation, SQLite persistence, log/topic association, and
  operation export/readback.
- Compare the git API with the stable API used by `PandaFactStore`.
- Decide whether production dependencies can move to the current git API in
  this slice.
- If they can move, remove the `p2panda-core-git` /
  `p2panda-store-git` alias split and make the production path compile on the
  current API line.
- If they cannot move, leave production dependencies on crates.io 0.5.2 and
  write the blocker into the report with the exact API/authority reason.

Execution note:

- Characterization-first. Do not start by rewriting `PandaFactStore`.

Test scenarios:

- A git-API operation can be written, persisted, reopened, and associated with
  the expected topic/log.
- If production dependencies are migrated, existing unit tests prove
  `write_fact`, `import_operation`, conflict candidates, payload reads, SQLite
  reopen, and sync still behave identically.
- If production dependencies are not migrated, a test or report captures the
  incompatible type/API boundary so the decision is reproducible.

Verification:

- `cargo test -p mvp-p2panda-facts --lib`

### Unit 2: Safe p2panda-net Sync Import Proof

Files:

- `MVP/p2panda-facts/src/lib.rs`
- `MVP/e2e/src/p2panda_net_sync_contract.rs`
- `MVP/e2e/src/main.rs`
- `MVP/e2e/Cargo.toml`

Work:

- Add the smallest p2panda-net-backed local sync proof that exchanges
  operations between two local nodes over p2panda-net's iroh/gossip/log-sync
  stack.
- Route received operations through the existing Ployz import validation path
  before they become projection-visible canonical facts.
- If p2panda-net's default store cannot be safely used as canonical truth,
  make that explicit with a quarantine/import bridge or stop at a narrower
  executable API proof.
- Preserve duplicate, conflict, writer-grant, trusted replica, and cross-island
  rejection semantics.
- Record sync latency, operation counts, duplicate no-op import counts,
  projection rebuild duration, and explicit deadline behavior.

Execution note:

- Authority-first. A passing network sync that bypasses Ployz import validation
  is a failing slice.

Test scenarios:

- Node A writes a signed p2panda fact, p2panda-net transports it, Node B imports
  it through Ployz validation, and Node B projection sees it.
- Repeating the sync is a no-op and does not create duplicate candidates.
- A same-key race transported over the network remains two candidates, with the
  reducer choosing deterministically.
- A remote operation from an untrusted author key is rejected and remains
  invisible to projection.
- A cross-island session cannot read candidates or payloads after network
  transport.
- All network waits have deadlines and report structured failure rather than
  hanging the E2E suite.

Verification:

- `cargo run -p mvp-e2e -- p2panda-net-sync-contract`
- `cargo run -p mvp-e2e -- p2panda-sync-fact-source-contract`

### Unit 3: Delete Or Demote Redundant Substrate

Files:

- `MVP/p2panda-facts/src/lib.rs`
- `MVP/e2e/src/p2panda_sync_fact_source_contract.rs`
- `MVP/e2e/src/p2panda_acme_http01_contract.rs`
- `MVP/e2e/src/deploy_restart_recovery_contract.rs`

Work:

- Identify any Ployz-owned sync/export/import code made redundant by Unit 1 or
  Unit 2.
- Delete genuinely redundant code, or rename/document remaining manual
  export/import helpers as deterministic harness/debug plumbing.
- Keep deterministic helpers only when an existing proof still needs targeted
  failure injection that p2panda-net cannot provide cleanly.
- Avoid broad refactors of business logic. The simplification target is the
  substrate boundary.

Execution note:

- Simplify in a separate commit after behavior is green.

Test scenarios:

- Deploy restart recovery and ACME still use the approved p2panda sync boundary
  and do not fall back to a feature-specific operation-copy loop.
- Manual helpers, if retained, are named or documented so future product code
  does not mistake them for the production sync path.

Verification:

- `cargo run -p mvp-e2e -- p2panda-acme-http01-contract`
- `cargo run -p mvp-e2e -- deploy-restart-recovery-contract`

### Unit 4: Time-Budgeted E2E And Metrics

Files:

- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/metrics.rs`
- `MVP/e2e/src/p2panda_net_sync_contract.rs`
- `MVP/e2e-proof-plan.md`

Work:

- Ensure the new scenario participates in the default all-run only if it is
  deterministic and bounded.
- Keep `MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all` as a required
  gate.
- Write metrics for network sync duration, import counts, repeated-sync no-op,
  projection rebuild, and any explicit timeout path.
- Compare p2panda-net sync timings against the Slice 020 in-process sync
  baseline without requiring exact parity.

Test scenarios:

- The full E2E suite finishes within the configured wall-clock budget.
- A forced unavailable/timeout path reports a structured error and exits
  promptly.
- Metrics are written for successful and blocked substitution outcomes.

Verification:

- `MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all`

### Unit 5: Decisions, Report, And Semantic Leverage

Files:

- `MVP/overall-plan.md`
- `MVP/architecture.md`
- `MVP/primitive-decisions.md`
- `MVP/e2e-proof-plan.md`
- `MVP/slice-022-p2panda-net-current-api-substitution.md`

Work:

- Add a "Changed Since Last Slice" entry to `MVP/primitive-decisions.md`.
- Record whether current p2panda APIs replaced production code, remained a
  dev/test transport proof, or were rejected for a specific reason.
- Update E2E proof status for transport-bound p2panda sync.
- Include a LOC/maintenance-burden comparison:
  - code deleted or demoted,
  - new code added,
  - custom substrate still owned by Ployz,
  - business-code surfaces made smaller or unchanged.
- Explicitly state whether this slice improved semantic leverage or only
  clarified a migration blocker.

Verification:

- Documentation references use repo-relative paths.
- The report links back to this plan and names the exact verification commands
  that passed.

## Review Risks

- p2panda-net may insert remote operations into its store before Ployz grants
  are checked. Treat this as an authority violation unless the store is
  explicitly quarantined.
- The git p2panda API line may force broad rewrites without enough deletion.
  Do not accept a churn-only migration.
- Network tests may become flaky if they rely on discovery timing. Prefer local
  explicit bootstrap and deadlines.
- Live mode can hide duplicate/stale delivery bugs. Keep it out unless the
  proof can force shutdown and repeat delivery deterministically.
- The current `MVP/p2panda-facts/src/lib.rs` file is already large. If this
  slice adds more substrate there, split only along clear existing boundaries
  and keep the simplify pass separate from behavior commits.

## Verification Summary

Required before closing the slice:

```text
cargo fmt --all
cargo test -p mvp-p2panda-facts --lib
cargo run -p mvp-e2e -- p2panda-net-sync-contract
cargo run -p mvp-e2e -- p2panda-sync-fact-source-contract
cargo run -p mvp-e2e -- p2panda-acme-http01-contract
cargo run -p mvp-e2e -- deploy-restart-recovery-contract
cargo clippy -p mvp-p2panda-facts -p mvp-e2e --all-targets -- -D warnings
MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

If the executable proof lands under the narrower
`p2panda-net-api-substitution-contract` name because safe canonical import is
blocked, update the command list and report with that name. Do not silently skip
the network/current-API proof.
