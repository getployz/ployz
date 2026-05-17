---
title: Slice 007 Iroh Toolchain And Docs Integration Plan
status: active
created: 2026-05-17
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-005-fact-projection.md
---

# Slice 007 Iroh Toolchain And Docs Integration Plan

## Problem Frame

The MVP has proved the bus, authority, bridge, fact, projection, snapshot, and
scale contracts in memory. That was the right first move, but it has reached
the limit of what an in-memory harness can prove. The architecture depends on
iroh connectivity and iroh-docs anti-entropy; keeping them as a future adapter
would let later product slices build on contracts that might not survive the
real substrate.

The single proof target for this slice is:

> `MVP/` adopts a concrete iroh-family version and Rust toolchain policy, then
> proves a minimal two-node iroh-docs fact path behind the existing projection
> seam without leaking raw iroh APIs into business logic.

This slice should close the "stop deferring iroh" gap. It is still not the full
machine-join, WireGuard, ACME, or deploy slice. It is the substrate proof those
product slices need before they can be credible.

## Why This Is Next

ACME is the next product canary, but it is blocked on the operator's singleton
primitive choice. Deploy commit-before-drain should come after ACME, and it
needs real fact durability/pinning semantics. The unblocked and highest-risk
foundation work is therefore iroh itself:

- current iroh-family crates have moved to a Rust version newer than the MVP
  declares,
- iroh-docs has its own model of namespace, author, key, content hash, content
  blobs, sync events, and conflicts,
- the projection reducer already has the right `FactSource` seam, but no real
  docs-backed adapter has proved it,
- later slices should not have to rediscover whether to pin old crates or raise
  the MVP toolchain.

This slice makes the version/toolchain decision explicit, proves it by
compiling and running against the real crates, and records the cost for future
maintainers.

## Requirements Traceability

- `VISION.md`: the data plane outlives the control plane; live observation must
  not fabricate stored truth; commands need bounded effects and visible
  failures.
- `MVP/overall-plan.md`: each slice must run a fresh planning pass, remain
  under `MVP/`, check existing crates before writing plumbing, and stop
  deferring iroh/toolchain decisions.
- `MVP/architecture.md`: iroh provides endpoint identity, QUIC streams, ALPN
  routing, and docs/blobs/gossip coexistence; iroh-docs is the durable
  replicated fact set; SQLite remains a projection.
- `MVP/e2e-proof-plan.md`: E2E-4 remains open until real docs-backed
  anti-entropy, fact propagation metrics, and remote projection proof exist.
- `MVP/primitive-decisions.md`: the in-memory fact harness is a contract
  backend, not the final replicated store; point-read conflicts and projection
  candidate status must match CRDT reality.
- `MVP/slice-005-fact-projection.md`: `mvp_projection::FactSource` is the seam
  real docs-backed entries should satisfy.

## Scope

Implement an MVP-local iroh integration slice:

- change the Rust version only in `MVP/Cargo.toml`; do not change root
  workspace or CI toolchains,
- add current iroh-family dependencies in a new MVP-local crate,
- create an `mvp-iroh` crate that owns iroh endpoint/docs/blobs/gossip setup
  for the tests and adapter code in this slice,
- expose a narrow Ployz-shaped docs fact adapter instead of raw iroh handles,
- write one fact payload through node A, sync it to node B through iroh-docs,
  list it through the `FactSource` contract, and rebuild projection state using
  the existing reducer/SQLite/snapshot path,
- record docs sync observations separately from fact truth,
- add timeout policy hooks so tests do not wait on production networking
  deadlines,
- update the E2E runner with an `iroh-docs-contract` scenario and include it in
  `all`,
- update `MVP/primitive-decisions.md` with the chosen version/toolchain and the
  real adapter boundary.

Out of scope:

- changing the root workspace Rust version or GitHub Actions toolchain,
- modifying existing `crates/` code,
- implementing machine invite/join,
- implementing WireGuard reconciliation,
- implementing ACME singleton semantics,
- implementing deploy commit-before-drain,
- implementing full PloyzBus-over-iroh request/reply,
- using iroh-docs as consensus or a mutable SQL-like database,
- replacing projection reducers or snapshot schemas.

Non-closure gate:

- this slice advances the docs-backed subset of E2E-4 only for local two-node
  anti-entropy and projection. It must not claim full E2E-4 closure, membership
  proof, or deployed transport proof.
- no report should imply NAT traversal, relay fallback, or multi-machine
  discovery is proven unless the slice actually exercises those paths.
- remote service-registry projection, p50/p95/p99 propagation metrics across
  many peers, machine join, and production persistence remain open after this
  slice unless explicitly added and tested.

## Current State

Current MVP workspace:

- `MVP/Cargo.toml` declares Rust `1.88` and has three crates: `bus`, `projection`,
  and `e2e`.
- `MVP/bus` owns the in-memory bus, grants, bridge rules, Kameo
  `BusActorHandle`, and in-memory fact harness.
- `MVP/projection/src/source.rs` defines `FactSource`.
- `MVP/projection/src/bus_source.rs` adapts the in-memory bus to `FactSource`.
- `MVP/e2e/src/main.rs` owns scenario dispatch and the time-budgeted `all`
  runner.
- `MVP/justfile` runs fmt, clippy, tests, and all E2E scenarios under
  `MVP_E2E_ALL_TIMEOUT`.

The installed local toolchain is newer than the declared minimum:

```text
rustc 1.95.0
cargo 1.95.0
```

The root workspace still declares Rust `1.88`, and `.github/workflows/pr.yml`
installs Rust `1.88`. Because this rewrite remains isolated under `MVP/`, the
slice should make an MVP-local decision and explicitly document that migration
outside `MVP/` will need a separate root toolchain decision.

## Crate Scout

Checked current crates on 2026-05-17:

- `iroh@1.0.0-rc.0`: current iroh line, Rust `1.91`. Provides peer-to-peer
  QUIC endpoints, endpoint IDs, relay-assisted connectivity, ALPN, cheap
  streams, and router support.
- `iroh-docs@0.99.0`: current docs line, Rust `1.91`. Provides signed
  namespace/author entries, BLAKE3 content hashes, redb-backed storage, range
  set reconciliation, live sync events, and imports/tickets. It composes with
  iroh, iroh-blobs, and iroh-gossip.
- `iroh-blobs@0.101.0`: current blob line, Rust `1.91`. Provides
  content-addressed blob storage and transfer for docs payload content.
- `iroh-gossip@0.99.0`: current gossip line, Rust `1.91`. Provides the gossip
  protocol docs uses for live update dissemination.
- Older compatible line: `iroh-docs@0.95.0`, `iroh@0.95.1`,
  `iroh-gossip@0.95.0`, and `iroh-blobs@0.97.0` work with Rust `1.85`, but
  they are a pre-1.0 API line and would create an immediate migration tax.
- `irpc@0.15.0` and `irpc-iroh@0.15.0`: relevant to future PloyzBus-over-iroh
  request/reply, but not needed for the first docs-backed fact source proof.
- `tokio-util::sync::CancellationToken`: useful for future endpoint/docs actor
  shutdown. Defer unless the implementation needs it; do not add cancellation
  surface before there is owned async lifecycle to cancel.

Decision for this plan:

- bump `MVP/` to Rust `1.91`,
- adopt the current aligned iroh stack (`iroh 1.0.0-rc.0`,
  `iroh-docs 0.99.0`, `iroh-blobs 0.101.0`, `iroh-gossip 0.99.0`),
- keep the root workspace at Rust `1.88`,
- isolate all new iroh code in `MVP/iroh`.

Rationale:

- The MVP is explicitly experimental and isolated.
- The local toolchain already supports this.
- Pinning the older line would prove integration against an API the project
  already expects to replace.
- A current iroh stack gives product slices the best signal about real API
  shape and failure modes.

External references:

- `iroh` crate docs:
  <https://docs.rs/iroh/1.0.0-rc.0/iroh/>
- `iroh-docs` crate docs:
  <https://docs.rs/iroh-docs/0.99.0/iroh_docs/>
- `iroh-blobs` crate docs:
  <https://docs.rs/iroh-blobs/0.101.0/iroh_blobs/>
- `iroh-gossip` crate docs:
  <https://docs.rs/iroh-gossip/0.99.0/iroh_gossip/>

## Key Technical Decisions

### Iroh Is Owned By A New MVP Crate

Create `MVP/iroh` with library name `mvp_iroh`.

The crate should own only abstractions consumed by this slice:

- endpoint/docs/blobs/gossip setup for local tests,
- iroh-docs document creation/import/sync helpers,
- Ployz fact key and payload encoding into docs entries,
- mapping docs entries into `mvp_projection::FactCandidate`,
- sync/probe status that distinguishes transport observation from durable fact
  truth,
- test timeout policy.

Anything only useful for later actors should stay private or wait for the actor
slice that needs it.

It should not own projection reducers, bus authority, deploy state machines, or
serving snapshots.

### Current Iroh Stack Wins Over Older Rust Compatibility

`MVP/` should move from Rust `1.88` to Rust `1.91`. This is deliberately not a
root workspace migration. The plan should leave root `Cargo.toml` and GitHub
Actions alone.

The implementation report must say this plainly:

- MVP local proof now requires Rust `1.91+`,
- root project still requires Rust `1.88`,
- migrating MVP code into `crates/` will require a separate root MSRV/CI
  decision.

### The Adapter Implements `FactSource`, Not A New Reducer Path

The docs-backed source must satisfy the existing projection contract:

```text
FactSource::list_candidates(...)
FactSource::read_payloads(...)
```

The reducer and SQLite/snapshot writers should not know whether facts came from
the in-memory bus or iroh-docs. Any implementation pressure to special-case
iroh inside reducers is a design failure for this slice.

`FactSource` is synchronous today, and `ProjectionActor` calls it inside a
blocking worker. The docs-backed adapter should therefore maintain a
synchronous local view of the already-open local docs replica. Async endpoint,
router, docs sync, and blob transfer lifecycle stays behind `mvp-iroh` handles;
projection reads a local snapshot/view with bounded lock/blocking behavior. Do
not make projection own a Tokio runtime, call async iroh APIs from inside
`spawn_blocking`, or change the reducer seam unless implementation proves the
current seam is impossible.

### Docs Access Is Not Authority

Possessing an iroh-docs namespace, endpoint address, or sync path is replication
access. It is not Ployz authority.

This slice needs one small bus-surface addition before `mvp-iroh` can enforce
that rule. Today `Grant::can_read_fact`, `Grant::can_write_fact`, and
`GrantBook` checks are `pub(crate)` inside `mvp-bus`. Do not duplicate those
rules in `mvp-iroh`. Add a narrow public authorizer contract in `mvp-bus` that
can answer only fact-read and fact-write questions for a session/principal/key,
then have the docs adapter depend on that contract.

The adapter must be configured with an explicit binding such as:

```text
NamespaceBinding {
    island: IslandId,
    docs_namespace: DocsNamespaceId,
    allowed_authors: BTreeMap<DocsAuthorId, PrincipalId>,
}
```

`CandidateStatus::Verified` requires all of:

- the docs entry is valid for the bound namespace,
- the docs author maps to a Ployz `PrincipalId`,
- the mapped principal has the relevant fact-read/projection grant for the
  requested island/key,
- the payload identity validates against the candidate fact key.

Unknown authors, unmapped namespaces, and entries from authors without Ployz
permission must be reported as `Unauthorized`/`CrossIsland` or redacted
according to the existing projection status vocabulary. They must never
auto-create principals.

Local docs fact writes that are not private test fixtures must also take an
explicit `BusSession` or equivalent Ployz authority context and check
`fact_write` permission before creating a docs entry or blob. Raw iroh-docs
write helpers should stay private or be named as test fixtures so product code
cannot bypass grants.

### Fact Truth And Sync Observation Stay Separate

iroh-docs live events and endpoint health are observations. They can wake
projection and feed operator status, but they cannot replace durable fact truth.

The adapter should expose two separate concepts:

- candidate facts from the local docs replica,
- sync/probe status such as last sync completion, peer count, timeout, or
  unavailable peer.

Tests should assert that a sync timeout produces a structured observation
failure without deleting or rewriting already-stored local fact candidates.

### Local Writes Do Not Round Trip Through Remote Iroh

A local command writing a fact records local durable truth first. iroh-docs
sync is propagation and anti-entropy, not the local mutation path.

This follows the prior self-target drain learning: choose local mutation vs
remote coordination from target identity and current state, not from a broad
command enum.

iroh endpoint identity is only connectivity identity in this slice. It may be
included in sync/probe observations, but it must not satisfy author
verification, grant lookup, namespace binding, or `PrincipalId` construction.

### Conflicts Are Candidate Status, Not Write-Time Rejection

iroh-docs can replicate multiple authors' entries for the same Ployz fact key.
The docs-backed source should group by `(island, fact_key)` and mark duplicate
content candidates as `CandidateStatus::Conflict`, matching the updated
in-memory harness.

The implementation should preserve the current bounded-conflict lesson where it
controls local writes, but it cannot assume a remote CRDT set contains only one
alternate. Reducer-facing status must remain robust even if the docs replica
contains more conflict candidates than the in-memory harness would admit.

## Implementation Units

### U1: MVP Toolchain And Dependency Decision

Files:

- Modify `MVP/Cargo.toml`
- Modify `MVP/Cargo.lock`
- Modify `MVP/primitive-decisions.md`
- Modify `MVP/bus/src/grants.rs`
- Modify `MVP/bus/src/lib.rs`
- Create `MVP/iroh/Cargo.toml`
- Create `MVP/iroh/src/lib.rs`

Goal:

- Move `MVP/` to Rust `1.91`.
- Add `mvp-iroh` to the workspace.
- Add current iroh-family dependencies only to `mvp-iroh`.
- Keep `mvp-bus` and `mvp-projection` free of direct iroh dependencies unless
  a later review proves that coupling is necessary.
- Expose a narrow fact-authorizer contract from `mvp-bus` so external fact
  backends can reuse grant semantics without reaching into bus internals.

Patterns to follow:

- Existing MVP workspace dependency style.
- Keep package names `mvp-*` and library names `mvp_*`.
- Use structured errors; avoid stringly public failures except at backend
  wrappers.

Test scenarios:

- `cargo check --all` compiles with the selected versions.
- `cargo tree -p mvp-iroh` shows one aligned iroh family, not mixed `0.95` and
  `0.99/1.0` lines.
- Existing `mvp-bus` and `mvp-projection` unit tests still pass.
- fact-authorizer tests prove read/write allow and deny semantics match the
  existing bus fact operations.

Execution note:

- Implementation may start with compile-smoke scaffolding before full adapter
  behavior, but do not commit a toolchain bump that does not compile.

### U2: Local Iroh Docs Harness

Files:

- Modify `MVP/iroh/src/lib.rs`
- Create supporting modules under `MVP/iroh/src/` as needed
- Add tests under `MVP/iroh/src/` or `MVP/iroh/tests/`

Goal:

- Spawn two local in-process iroh endpoints with docs, blobs, gossip, and router
  protocols.
- Avoid public relay/DNS dependency in tests; use direct local endpoint
  addresses where possible.
- Create or share a docs namespace from node A to node B.
- Write one payload-backed docs entry on node A and observe it on node B after
  bounded sync.
- Maintain a synchronous local docs view that can be read by `FactSource`
  without async calls in projection workers.
- Shutdown owned handles cleanly.

Patterns to follow:

- `MVP/e2e/src/main.rs` budget handling: tests own their wall-clock budget.
- Prior timeout learning: inject short test policies rather than changing
  production defaults.
- Actor ownership direction from `MVP/architecture.md`, but do not introduce a
  Kameo actor until there is real state that benefits from actor ownership.

Test scenarios:

- two local nodes sync one docs entry within a small injected deadline,
- an offline peer returns a structured timeout/unavailable observation,
- local docs entries remain listable after the remote sync attempt fails,
- synced local docs entries are visible through the synchronous local view,
- shutdown does not hang.

Execution note:

- Use real iroh APIs. Avoid replacing this with a fake transport because the
  point of the slice is to bind the MVP to the actual substrate.

### U3: Docs-Backed FactSource Adapter

Files:

- Modify `MVP/iroh/src/lib.rs`
- Create `MVP/iroh/src/facts.rs` or equivalent
- Modify `MVP/iroh/Cargo.toml`
- Modify `MVP/bus/src/grants.rs` and `MVP/bus/src/lib.rs` if U1 did not finish
  the fact-authorizer contract
- Modify `MVP/e2e/Cargo.toml`

Goal:

- Encode Ployz fact payload bytes into docs/blobs.
- Map docs entries back into `FactCandidate` values.
- Read payloads by content hash for candidates.
- Preserve island scoping through explicit adapter configuration, not by
  embedding island names into subject strings or fact keys.
- Surface unverified, unauthorized, cross-island, and conflict candidates in
  the same status vocabulary used by `mvp-projection`.
- Require an explicit session/principal/island context and fact-write grant for
  any public docs-backed fact write.
- Keep namespace access, endpoint identity, author identity, and Ployz
  principal authority as separate values.
- Do not expose a public hash-only blob read path.

Patterns to follow:

- `MVP/projection/src/bus_source.rs` grouping for conflicts.
- `MVP/projection/src/source.rs` trait contract.
- `MVP/bus/src/facts.rs` fact key/hash types and payload identity rules.

Test scenarios:

- verified docs entry projects as `CandidateStatus::Verified`,
- same key with two different content hashes projects both as
  `CandidateStatus::Conflict`,
- candidate from an unmapped namespace is ignored or marked cross-island
  without being projected into the requested island,
- unauthorized author is reported as unauthorized and redacted from projection
  outputs,
- payload read by hash cannot cross island/key adapter boundaries.
- a docs-backed write without fact-write grant fails before durable docs/blob
  mutation,
- iroh endpoint ID cannot be used as a principal substitute.

Execution note:

- If the iroh-docs API makes some status class impossible to produce directly,
  model it at the adapter boundary with explicit test fixtures rather than
  deleting the projection status vocabulary.
- Unverified, unauthorized, cross-island, conflict, and redaction coverage in
  this slice should use explicit adapter-local bindings/fixtures. Do not build a
  real replicated principal registry, membership subsystem, invite flow, or
  operator-editable policy engine here.
- `read_payloads` may only fetch blobs for candidates returned by the same
  adapter binding/session, must verify bytes match the candidate content hash,
  and must validate payload identity against the candidate fact key before
  returning bytes to projection.

### U4: E2E Iroh Docs Contract

Files:

- Create `MVP/e2e/src/iroh_docs_contract.rs`
- Modify `MVP/e2e/src/main.rs`
- Modify `MVP/e2e/Cargo.toml`
- Modify `MVP/justfile` only if the all-scenario timeout needs a justified
  bump

Goal:

- Add `cargo run -p mvp-e2e -- iroh-docs-contract`.
- Include it in `all`.
- Prove two local iroh nodes sync a fact, node B projects it through the same
  reducer/SQLite/snapshot path, and projection can rebuild after deleting
  SQLite.
- Emit metrics for sync duration, projection duration, entries observed,
  conflicts ignored, and timeout policy.

Test scenarios:

- two-node fact propagation through iroh-docs,
- projection rebuild from node B's docs-backed source after deleting SQLite,
- dropped/live event not required for correctness; explicit full projection pass
  still catches up from local docs replica,
- sync timeout reports observation failure and preserves last known local docs
  entries.

Execution note:

- Keep the E2E scenario small and deterministic. This is not the 10k scale path.
  The existing scale test remains in-memory because it proves bus/projection
  logical margin; this slice proves real substrate correctness at small scale.

### U5: Maintainer Docs And Slice Report

Files:

- Modify `MVP/primitive-decisions.md`
- Create `MVP/slice-007-iroh-toolchain-integration.md`

Goal:

- Record the final Rust/toolchain decision.
- Record the iroh-family versions and why the older compatible line was not
  chosen.
- Explain that raw iroh types may escape only into `mvp-iroh` internals and E2E
  harness setup, not business logic or reducers.
- Record proof results and remaining gaps.

Test scenarios:

- Documentation names exact commands run and whether `just test` passed.
- Documentation does not claim NAT traversal, relay fallback, machine join, or
  production persistence beyond what the slice actually proves.

## Verification

Minimum targeted checks:

```text
cd MVP && cargo check --all
cd MVP && cargo test -p mvp-iroh
cd MVP && cargo test -p mvp-projection
cd MVP && cargo run -p mvp-e2e -- iroh-docs-contract
cd MVP && cargo run -p mvp-e2e -- projection-contract
```

Pre-push gate:

```text
cd MVP && just test
```

The `just test` run must remain time-budgeted. If real iroh startup pushes the
default `MVP_E2E_ALL_TIMEOUT=120s` budget, the implementation must first
diagnose whether the delay is expected setup cost, network leakage, or a bug.
Only then may it adjust the budget with an explicit plan/report note.

## Review Risks

Ask reviewers to focus on:

- toolchain blast radius: ensure root workspace Rust `1.88` and CI are not
  changed accidentally,
- iroh leakage: business logic, reducers, and existing bus/projection crates
  should not depend on raw endpoint/docs types,
- fact truth vs observation: sync failures must not rewrite durable facts or
  projections,
- timeout discipline: no iroh connect/sync/shutdown path can await forever,
- conflict handling: docs-backed duplicate keys must remain reducer-visible,
- test determinism: tests should use local addresses and scoped deadlines, not
  public relay/DNS availability.

## Semantic-Leverage Check

The business rule this slice should make easy:

> A route/service/node fact replicated by iroh-docs can be projected by the
> same business reducer as an in-memory fact.

Evidence to report:

- number of projection reducer files changed,
- number of raw iroh types exposed outside `mvp-iroh`,
- E2E test shape: whether it reads as "write fact, sync fact, project fact" or
  as transport choreography,
- whether a future product slice can consume a docs-backed `FactSource` without
  learning iroh-docs APIs.

Expected result:

- reducer files should not need behavior changes,
- raw iroh types should be confined to `mvp-iroh` and E2E harness setup,
- the contract test should reuse projection APIs from slice 005.

## Open Questions Deferred To Implementation

- The exact minimal local endpoint builder API for relay-free tests. Prefer
  iroh's minimal/local builder and direct `EndpointAddr`; do not rely on public
  relay or DNS for local tests.
- The exact docs entry key encoding. It should be deterministic and reversible
  to `FactKey` without normalizing away conflict information.
- The exact Rust type names for `NamespaceBinding` and docs author IDs. The
  semantic rule is not deferred: docs authors map through explicit adapter
  configuration to Ployz principals, and unknown authors do not become
  principals automatically.
- Whether `mvp-iroh` needs an actor in this slice. Add one only if endpoint/docs
  lifecycle ownership becomes complex enough to justify it.

## Future Work After This Slice

- ACME singleton primitive, once the operator chooses queue-group singleton,
  lease fact, or named singleton service.
- Deploy commit-before-drain using docs-backed route commit facts and
  `store.pin_fact`.
- PloyzBus-over-iroh request/reply and bridge transport.
- Machine invite/join with iroh endpoint identity and docs access.
- WireGuard full-mesh reconciliation from docs-backed node facts.
- HTTP/DNS serving-state process proof with coordinator down.
