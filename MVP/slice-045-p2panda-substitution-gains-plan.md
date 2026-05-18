---
title: Slice 045 p2panda Substitution Gains Investigation Plan
status: active
created: 2026-05-19
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/semantic-leverage-loc.md
  - MVP/slice-037-p2panda-06-substrate-deletion.md
  - MVP/slice-039-p2panda-substitution-deletion-audit.md
  - MVP/slice-040-delete-opaque-p2panda-net-transport.md
  - MVP/slice-041-p2panda-auth-membership-substitution.md
  - MVP/slice-044-membership-backed-deploy-recovery-plan.md
external:
  - https://docs.rs/p2panda-net
  - https://docs.rs/p2panda-auth
  - https://docs.rs/p2panda-store/latest/p2panda_store/
  - https://docs.rs/p2panda-sync
  - https://p2panda.org/2025/07/09/streams-transactions-crash-resilience.html
  - https://p2panda.org/2025/07/28/access-control.html
  - https://p2panda.org/2025/08/27/notes-convergent-access-control-crdt.html
---

# Slice 045 p2panda Substitution Gains Investigation Plan

## Problem Frame

The operator explicitly wants the next slice to investigate the largest
simplification gains available from substituting maintained p2panda crates
early, with a bias toward using them. That matters because the MVP is proving
whether Ployz can get much more business logic per line than the old codebase.
If maintained p2panda primitives can replace MVP-local fact, transport,
membership, replay, or process-role plumbing, the plan should identify those
swaps before more product features are built on top of local substrate.

This is an investigation slice, not a product-feature slice. Its output should
be a concrete substitution map and next implementation recommendation, backed
by repo inventory, upstream crate/API evidence, and targeted compile/runtime
probes where local evidence is cheaper than speculation.

The slice must preserve Ployz's product-owned boundaries from `VISION.md`:
explicit operator commands, no hidden reconcilers, no quorum/witness commit
semantics, visible nodes at decision time, and data-plane/process-role behavior
that survives coordinator death.

## Current Evidence

Repo evidence:

- `cargo tree --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport -i p2panda-net`
  shows `mvp-p2panda-transport` already uses `p2panda-net v0.6.0`.
- `cargo tree --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts` shows
  `mvp-p2panda-facts` and `mvp-p2panda-authz` already use p2panda `0.6.0`
  crates, including `p2panda-core`, `p2panda-store`, `p2panda-stream`,
  `p2panda-sync`, and `p2panda-auth`.
- `rg` still finds manual trust and direct import APIs in core fact-store
  fallback code, unit tests, `mvp-machine-p2panda` helper surfaces,
  `mvp-commands-p2panda` tests, `volume_transfer_contract`, and targeted
  p2panda-net fallback fixtures.
- Slice 044 moved deploy restart recovery onto membership-backed authority.
  The remaining product-shaped manual trust candidate is volume transfer.

Upstream evidence:

- `p2panda-net` describes itself as data-type-agnostic event delivery with
  iroh-backed encrypted transport, confidential topic discovery, gossip,
  log sync, address book, and optional supervision.
- `p2panda-auth` provides decentralised group management with `Pull`, `Read`,
  `Write`, and `Manage` levels, eventually consistent group state, strict
  manager-only group modification, and customizable conflict resolution.
- `p2panda-store` provides operation/log store interfaces plus SQLite-backed
  `OperationStore` and `LogStore`, but explicitly does not validate log
  integrity; Ployz must keep validation and product authorization above it.
- `p2panda-sync` provides protocol and manager traits plus log-sync
  implementations; it is lower-level than `p2panda-net` and useful where Ployz
  wants deterministic sync without full network modules.
- p2panda's stream/transaction/crash-resilience writing points toward stream
  controllers and operation acknowledgement as future replacement candidates
  for some local replay/status mechanics, but this is not yet a direct drop-in
  until the APIs exist in the crates Ployz uses.

## Bias And Boundaries

Bias toward using p2panda:

- Prefer deleting local wrappers around operation transport, persistent logs,
  group membership replay, and sync orchestration when p2panda crates already
  provide an equivalent maintained primitive.
- Prefer compile-backed or E2E-backed substitution decisions over deferring
  because a crate is young. MVP-local AI-written substrate is not inherently
  safer than maintained upstream substrate.
- Treat `p2panda-net 0.6.0` on non-RC iroh as available. Do not reopen the
  "must use RC iroh" blocker.

Do not outsource these Ployz-owned semantics:

- NATS-shaped `PloyzBus` subject, request/reply, queue group, bridge, and
  permission semantics.
- Ployz fact-key grants, command-entry conflict checks, and structured
  branchable errors.
- Projection reducers, gateway/DNS snapshot rules, and last-good serving.
- Deploy, machine, environment, ACME, volume, and future command state
  machines.
- Visible nodes at decision time and no-quorum/local-decision semantics.
- Machine tombstone/reinvite policy and WireGuard overlay policy.

## Scope

In scope:

- Inventory remaining MVP-local p2panda-adjacent code and classify it as:
  product-owned seam, replace-now candidate, delete-now candidate,
  fixture-only fallback, or defer.
- Investigate the largest substitution gains, especially:
  - volume transfer manual trust and E2E-local `PandaVolumeFactStore`;
  - `mvp-machine-p2panda` trust helper methods that may now be fixture-only;
  - `mvp-commands-p2panda` raw trusted-author test setup;
  - `mvp-p2panda-facts` manual trust/import APIs and whether they can be hidden
    behind test-only features or made private;
  - `mvp-p2panda-transport` fact-node stream flakiness and whether p2panda-net
    address book/supervisor APIs can replace local startup/retry scaffolding;
  - excluded `MVP/p2panda-06-spike` value now that the active workspace uses
    p2panda `0.6.0`;
  - historical iroh/process/bus fact-source scenarios that may no longer belong
    in `mvp-e2e -- all`.
- Produce an evidence-backed substitution ledger and next implementation slice
  recommendation.
- Update maintainer-facing docs with the decision.

Out of scope:

- Do not implement the large substitution in this slice unless the
  investigation discovers a trivial deletion that is required to make the
  evidence accurate.
- Do not migrate product commands to a new p2panda API while still
  investigating which swap has the highest leverage.
- Do not remove fallback/manual APIs used by active tests without first naming
  and preserving their fixture purpose or replacing their coverage.
- Do not change non-`MVP/` code.
- Do not use p2panda address book/discovery as durable membership or command
  consistency truth.

## Deliverables

- `MVP/slice-045-p2panda-substitution-gains.md`: final investigation report
  with substitution ledger, largest-gain recommendation, and proof evidence.
- `MVP/design-notes/p2panda-substitution-gains.md`: maintainer-facing summary
  of which p2panda crates should replace which local surfaces and why.
- Updates to:
  - `MVP/overall-plan.md`
  - `MVP/primitive-decisions.md`
  - `MVP/design-notes/semantic-leverage-loc.md`
  - `MVP/e2e-proof-plan.md` if test-list or proof ownership changes.
- Optional compile-backed notes or tiny tests only if needed to resolve a
  concrete substitution uncertainty.

## Implementation Units

### Unit 1: Remaining Local Plumbing Inventory

Files:

- `MVP/p2panda-facts/src/lib.rs`
- `MVP/p2panda-transport/src/fact_node.rs`
- `MVP/p2panda-transport/src/node.rs`
- `MVP/p2panda-transport/src/tests.rs`
- `MVP/p2panda-authz/src/lib.rs`
- `MVP/machine-p2panda/src/lib.rs`
- `MVP/commands-p2panda/src/lib.rs`
- `MVP/e2e/src/volume_transfer_contract.rs`
- `MVP/e2e/src/process_role_harness.rs`
- `MVP/e2e/src/p2panda_net_fact_node_contract.rs`

Plan:

1. Build a grep-backed inventory of remaining manual trust/direct import calls.
2. Classify each hit as product path, adapter helper, unit-test fixture,
   negative probe, fallback API, or obsolete historical path.
3. Count rough LOC for each candidate surface, using nonblank line counts where
   the result informs semantic leverage.
4. Record "delete", "replace with p2panda-auth", "replace with p2panda-net",
   "replace with p2panda-store/sync", "keep Ployz-owned", or "defer".

Test Scenarios:

- The inventory identifies every remaining call to:
  `trust_author_key`, `trust_replica_peer`, `with_trusted_author_key`,
  `from_trusted_authors`, and direct `import_operation(` outside core
  implementation/tests.
- The inventory separately lists p2panda-net stream/replay code and product
  command adapters so transport and business glue are not conflated.

Verification:

```text
rg -n "trust_author_key|trust_replica_peer|with_trusted_author_key|from_trusted_authors|import_operation\\(" MVP -g '*.rs'
rg -n "PandaNetFactNode|AddressBook|Discovery|Gossip|LogSync|Supervisor|PandaNetReplayCache|refresh_stream|StreamEnded" MVP/p2panda-transport MVP/e2e/src
```

### Unit 2: Upstream API Fit Matrix

Files:

- `MVP/slice-045-p2panda-substitution-gains.md`
- `MVP/design-notes/p2panda-substitution-gains.md`

Plan:

1. For each p2panda crate already in the workspace, document what it can replace
   today:
   - `p2panda-auth`: group membership, strong-removal membership replay,
     replica/write role classification.
   - `p2panda-store`: operation/log persistence and SQLite-backed durable logs.
   - `p2panda-sync`: deterministic log sync and protocol manager pieces.
   - `p2panda-net`: iroh transport, topic log sync, discovery/address book,
     gossip, optional internal supervision.
   - `p2panda-stream`: operation stream/rebuild helpers where currently used,
     plus future acknowledged processing if/when exposed.
2. Record what each crate cannot replace without weakening Ployz:
   fact-key grants, command consistency, projection reducers, visible nodes,
   last-good serving, and data-plane lifecycle.
3. Identify APIs that look promising but need a spike before adoption, such as
   address book/discovery health or supervisor integration.

Test Scenarios:

- The matrix must distinguish currently compiled dependencies from hypothetical
  future p2panda features.
- Each proposed substitution names a concrete Ployz file or behavior it would
  replace.

Verification:

```text
cargo tree --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts
cargo tree --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport
```

### Unit 3: Highest-Gain Candidate Selection

Files:

- `MVP/slice-045-p2panda-substitution-gains.md`
- `MVP/design-notes/semantic-leverage-loc.md`
- `MVP/overall-plan.md`
- `MVP/primitive-decisions.md`

Plan:

Compare candidates using these criteria:

- LOC/substrate deleted or made fixture-only.
- Product risk reduced.
- Future business-code semantics simplified.
- E2E coverage preserved or strengthened.
- Fit with "daemon can die, data plane keeps serving" goal.
- Fit with "operator connected node is the consistency boundary; no quorum"
  direction.

Candidate list to score:

1. Volume transfer membership-backed p2panda facts.
2. Manual trust API quarantine/deletion in `mvp-p2panda-facts` plus adapter
   helper cleanup.
3. p2panda-net fact-node reliability/supervision/address-book substitution.
4. Retiring historical non-product E2E fact-source scenarios from `all`.
5. Deleting or integrating `MVP/p2panda-06-spike`.
6. Moving command phase store tests to membership-backed fixtures.

Expected decision shape:

- Pick one primary next implementation slice.
- Name one secondary follow-up.
- Name at least one tempting substitution to reject/defer, with reason.

Test Scenarios:

- The selected next slice has a concrete proof gate that would fail against the
  current implementation.
- The recommendation does not require changing non-`MVP/` code.
- The recommendation preserves current product canary coverage.

### Unit 4: Proof And Documentation Closeout

Files:

- `MVP/slice-045-p2panda-substitution-gains-plan.md`
- `MVP/slice-045-p2panda-substitution-gains.md`
- `MVP/design-notes/p2panda-substitution-gains.md`
- `MVP/overall-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/design-notes/semantic-leverage-loc.md`
- `MVP/e2e-proof-plan.md`

Plan:

1. Record exact commands run and their outcomes.
2. Link the final recommendation from `overall-plan.md`.
3. Add a `Changed Since Last Slice` entry to `primitive-decisions.md`.
4. Keep the plan status `active` until the report and docs are complete, then
   mark it `completed`.
5. Commit the investigation separately from any incidental test cleanup.

Verification:

```text
cargo check --manifest-path MVP/Cargo.toml -p mvp-e2e
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-authz
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- volume-transfer-contract
git diff --check
```

## Review Focus

Run review after the investigation report, focused on:

- whether the recommendation is actually the highest-leverage substitution;
- whether it overclaims p2panda's responsibility and under-specifies Ployz-owned
  seams;
- whether any proposed deletion would remove unique E2E coverage;
- whether the next implementation slice has crisp failure/proof gates.

If subagent review is unavailable due usage limits, run the same review roles
locally and record that limitation in the slice report.

## Expected Follow-up

The likely implementation slice is either:

- volume transfer membership-backed facts, if product manual trust is the
  highest remaining semantic-risk target; or
- p2panda-net fact-node reliability/supervision/address-book hardening, if the
  recent `mvp-e2e -- all` flakes show transport substrate reliability is the
  bigger blocker to trusting the foundation.

The investigation decides which one earns the next slice.
