---
title: Slice 032 p2panda-net crates.io Substitution Plan
status: planned
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-030-p2panda-net-fact-node.md
  - MVP/slice-031-p2panda-net-process-serving.md
  - MVP/p2panda-transport/Cargo.toml
  - MVP/p2panda-transport/src/node.rs
  - MVP/p2panda-transport/src/fact_node.rs
  - MVP/e2e/src/p2panda_net_process_serving_contract.rs
external:
  - https://docs.rs/p2panda-net/latest/p2panda_net/
  - https://docs.rs/p2panda-auth/latest/p2panda_auth/
  - https://docs.rs/p2panda-blobs/latest/p2panda_blobs/
  - https://github.com/p2panda/p2panda
---

# Slice 032 p2panda-net crates.io Substitution Plan

## Problem Frame

Slices 030 and 031 proved the right transport shape: `p2panda-net` owns live
fact movement, while `SharedPandaFactStore` owns Ployz authorization, trusted
replica import, conflict candidates, projection reads, and domain semantics.

The remaining weak point is dependency shape. `mvp-p2panda-transport` still pins
git `p2panda-net`, `p2panda-core`, `p2panda-store`, and `p2panda-sync` at a
specific revision, while `mvp-p2panda-facts` uses crates.io `0.5.2` for the
canonical fact store and only pulls git p2panda APIs in dev dependencies. That
split made sense while `p2panda-net` looked unavailable or blocked by an iroh
version line. It is no longer the best default.

The user explicitly accepted avoiding RC iroh, and current crate scouting shows
`p2panda-net 0.5.2` is published on crates.io with default modules for address
book, endpoint, mDNS, discovery, gossip, and sync. It depends on the crates.io
p2panda `0.5.2` line and `iroh 0.96.1`, not the newer RC line.

Slice 032 should try to delete the git-pinned p2panda transport line now. If
the public API is compatible, this is a straight substitution. If it is not,
the slice should make the smallest adapter change needed and document the
remaining blocker precisely.

## Non-Negotiable Direction

Bias toward using p2panda crates.

The comparison is not "stable p2panda versus perfect Ployz substrate." It is
"maintained p2panda plumbing versus custom AI-written networking and sync
plumbing." If a p2panda crate does the job with acceptable architecture
boundaries, prefer it even if the API is pre-1.0.

Do not grow a parallel hand-rolled iroh fact sync path in this slice. The only
acceptable reasons to keep git p2panda dependencies are compile/API blockers
that are written down with exact symbols and error surfaces.

## Dependency Scout

Checked on 2026-05-18:

- `cargo info p2panda-net --verbose` reports `p2panda-net 0.5.2` on crates.io.
  Its default features include address book, iroh endpoint, mDNS, discovery,
  gossip, and sync. Its dependency line includes `p2panda-core 0.5.2`,
  `p2panda-store 0.5.2`, `p2panda-sync 0.5.2`, `iroh 0.96.1`, and
  `iroh-gossip 0.96.0`.
- The `p2panda-net` docs describe the exact module set the MVP needs:
  peer discovery, gossip for ephemeral online delivery, `LogSync` for
  eventually consistent append-only logs, address book management, and optional
  supervisor actors.
- The p2panda repository README says the crates are modular, operate over raw
  bytes, and are intended to let projects pick the pieces they need without
  framework lock-in. That matches the MVP's boundary: p2panda moves fact
  operation bytes; Ployz owns product authority and reducers.
- `p2panda-auth 0.5.2` looks promising for later island membership/replication
  grants, but it should not be pulled into this slice. The current slice is
  transport dependency substitution only.
- `p2panda-blobs 0.5.2` looks promising for later payload replacement because
  it integrates with `p2panda-net` and uses BLAKE3-verified streaming, but it
  should be a later product slice. Do not mix blob migration into this transport
  dependency cleanup.

## Scope

In scope:

- Replace git p2panda dependencies in `MVP/p2panda-transport/Cargo.toml` with
  crates.io `0.5.2` where the public API permits it.
- Remove `p2panda-core-git`, `p2panda-store-git`, and `p2panda-sync-git` aliases
  from production transport code if crates.io APIs compile.
- Remove git p2panda dev dependencies from `MVP/p2panda-facts/Cargo.toml` if
  their tests can use crates.io `p2panda-net 0.5.2` or if the tested behavior is
  already covered through `mvp-p2panda-transport`.
- Keep the Ployz-facing API stable: `PandaNetFactNode`, `PandaNetNode`,
  `PandaNetNodeTicket`, `PandaNetTopic`, `PandaNetNetworkId`,
  `PandaNetFactImportOutcome`, and process-role E2Es should not expose raw
  p2panda types.
- Update `MVP/primitive-decisions.md` and `MVP/overall-plan.md` with the
  crates.io substitution result.

Out of scope:

- No new product feature.
- No p2panda-auth island membership migration.
- No p2panda-blobs payload migration.
- No root-workspace dependency migration outside `MVP/`.
- No direct RC iroh adoption. If the crates.io p2panda-net path works, keep
  transport on that line. If it does not, document why before considering any
  iroh-line change.

## Implementation Units

### Unit 1: Compile-First Transport Dependency Swap

Files:

- `MVP/p2panda-transport/Cargo.toml`
- `MVP/p2panda-transport/src/node.rs`
- `MVP/p2panda-transport/src/quarantine_log.rs`
- `MVP/p2panda-transport/src/fact_node.rs`
- `MVP/Cargo.lock`

Plan:

1. Change `mvp-p2panda-transport` from git p2panda dependencies to crates.io
   `p2panda-net = "0.5.2"`, `p2panda-core = "0.5.2"`,
   `p2panda-store = { version = "0.5.2", features = ["sqlite", "macros"] }`,
   and `p2panda-sync = "0.5.2"` as needed.
2. Remove `*-git` import aliases from transport source.
3. Compile `mvp-p2panda-transport`.
4. If compile fails, classify each failure:
   - simple module/path rename,
   - missing feature flag,
   - behavioral API missing from crates.io,
   - incompatible p2panda operation type between transport and fact store.
5. Fix simple rename/feature issues in this slice. If a real behavior is absent
   from crates.io, keep the git dependency only for that symbol and document it
   in the slice completion notes.

Test scenarios:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport --all-targets`
- Existing unit tests must still prove ticket round-trip, owned node sync,
  envelope-size rejection, deferred import retry, pending queue full,
  duplicate/conflict import reporting, and trusted-replica enforcement.

### Unit 2: Remove Git p2panda Test Edges Where They No Longer Pay Rent

Files:

- `MVP/p2panda-facts/Cargo.toml`
- `MVP/p2panda-facts/src/lib.rs`
- `MVP/p2panda-facts/src/tests.rs`
- `MVP/Cargo.lock`

Plan:

1. Check whether `mvp-p2panda-facts` still needs direct dev access to raw
   `p2panda-net` or git store APIs.
2. If the tests are fact-store tests, keep them on the stable fact-store line
   and delete git p2panda dev dependencies.
3. If a network-oriented test remains, move that expectation to
   `mvp-p2panda-transport` or use crates.io `p2panda-net 0.5.2` as a dev
   dependency.
4. Do not weaken the authorization/import test surface just to remove a
   dependency. Deleting a dependency is only a win if the same behavior remains
   proved.

Test scenarios:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts --all-targets`
- Authorization rejection, duplicate facts, conflict candidates, operation
  export/import, sync replication, persistence reopen, and shared store clone
  behavior remain covered.

### Unit 3: Product E2E Regression Gate

Files:

- `MVP/e2e/src/p2panda_net_fact_node_contract.rs`
- `MVP/e2e/src/p2panda_net_process_serving_contract.rs`
- `MVP/e2e/src/process_role_harness.rs`
- `MVP/e2e-proof-plan.md`

Plan:

1. Run the two product contracts that prove the substituted transport still
   carries facts into Ployz projections:
   `p2panda-net-fact-node-contract` and
   `p2panda-net-process-serving-contract`.
2. Preserve existing metrics shape. Add a dependency-line field only if it helps
   future maintainers prove which transport line was exercised.
3. Run `MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all`.

Test scenarios:

- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-process-serving-contract`
- `MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all`

### Unit 4: Decision Ledger and Leverage Report

Files:

- `MVP/primitive-decisions.md`
- `MVP/overall-plan.md`
- `MVP/e2e-proof-plan.md`
- `MVP/slice-032-p2panda-net-crates-io-substitution.md`

Plan:

1. Record whether crates.io `p2panda-net 0.5.2` fully replaced git
   dependencies.
2. If any git dependency remains, name the exact API or behavior that requires
   it and the trigger to revisit.
3. Record the LOC/dependency leverage honestly:
   - git dependency count before/after,
   - Ployz transport wrapper LOC before/after,
   - whether product E2Es changed or stayed stable.
4. Update the "Changed Since Last Slice" section with the new dependency
   boundary.

## Review and Simplification

Run review after implementation with subagents because this touches transport
and dependency boundaries:

- Correctness review: API drift, lost import outcomes, stale pending-queue
  behavior, wrong status after process-role stream refresh.
- Maintainability review: whether the wrapper still hides raw p2panda types and
  whether any compatibility shim is broader than needed.
- Simplify pass: delete aliases, feature flags, helper wrappers, or tests that
  only existed for the git/crates.io split.

The simplify pass should be a separate commit if it changes code after the
implementation commit.

## Success Criteria

- `mvp-p2panda-transport` uses crates.io p2panda dependencies where public APIs
  permit it.
- No product/domain crate learns raw p2panda-net types.
- Existing p2panda-net E2Es pass without weakening assertions.
- The decision ledger clearly says whether git p2panda is gone or exactly why
  it remains.
- The slice does not introduce a new hand-rolled iroh transport path.
