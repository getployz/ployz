---
title: MVP Consolidation Plan
status: active
created: 2026-05-20
scope: duplicate-crate-cull-and-god-module-reduction
---

# MVP Consolidation Plan

## Problem Frame

Stop new product feature work until the MVP has one canonical implementation
per command and production modules have concept-backed boundaries. The current
workspace still carries parallel domain crates (`deploy` plus
`deploy-p2panda`, etc.) and several production files are large enough to expose
god-module risk. This makes every next product slice pay duplicate-crate and
unclear-ownership debt first.

This consolidation is not a file-splitting exercise. Each reduction must name
the missing abstraction, crate, or concept that explains why the original file
was too large.

The 1,500 LOC threshold is a smoke alarm and final guardrail, not the design
target. A file below that number can still be a god module if it owns too many
responsibilities or if the core concept is unreadable. A file move only counts
when it makes ownership, failure behavior, or domain rules easier to review.

## Current Evidence

Commands used for the current audit:

```text
find MVP -path '*/target/*' -prune -o -path 'MVP/e2e/*' -prune -o -type f -name '*.rs' -print0 | xargs -0 wc -l | sort -nr | head -40
rg -n "mvp-(commands|deploy|machine|environment|routing)-p2panda|commands-p2panda|deploy-p2panda|machine-p2panda|environment-p2panda|routing-p2panda|mvp_(commands|deploy|machine|environment|routing)_p2panda" MVP -g 'Cargo.toml' -g '*.rs'
```

Current production files above 1,500 physical lines:

| File | Current LOC | Required action |
| --- | ---: | --- |
| `MVP/p2panda-facts/src/lib.rs` | 4,784 | Reduce below 1,500 by extracting substrate concepts. |
| `MVP/bus/src/memory.rs` | 3,841 | Reduce below 1,500 by separating semantic state from delivery runtime. |
| `MVP/p2panda-authz/src/lib.rs` | 3,362 | Reduce below 1,500 by separating membership operation envelope, reducer adapter, and authority view. |
| `MVP/projection/src/reducer.rs` | 2,796 | Reduce below 1,500 through per-domain reducers behind one reducer contract. |
| `MVP/lease/src/lib.rs` | 1,863 | Reduce below 1,500 by separating lease domain model, ledger/reducer, and command surface. |
| `MVP/iroh/src/facts.rs` | 1,689 | Reduce below 1,500 or delete by retiring old direct-iroh fact wrapper paths. |
| `MVP/machine/src/remove.rs` | 1,658 | Reduce below 1,500 by separating remove state machine, participant protocol, and recovery/read model. |
| `MVP/node/src/membership.rs` | 1,634 | Reduce below 1,500 by extracting daemon control service and membership fact publication/admission concepts. |

Named file that is not currently over 1,500 physical lines but is included
because it was explicitly called out:

| File | Current LOC | Required action |
| --- | ---: | --- |
| `MVP/acme/src/lib.rs` | 1,158 | Keep under threshold; if modified, split by ACME order/challenge concept rather than line count. |

Duplicate crate inventory:

| Domain | Current semantic crate | Current p2panda adapter crate | Problem |
| --- | --- | --- | --- |
| Commands | `MVP/commands` / `mvp-commands` | `MVP/commands-p2panda` / `mvp-commands-p2panda` | Adapter depends on semantic command envelopes; E2E still names both surfaces. |
| Deploy | `MVP/deploy` / `mvp-deploy` | `MVP/deploy-p2panda` / `mvp-deploy-p2panda` | Adapter depends on semantic crate; callers depend on both. |
| Machine | `MVP/machine` / `mvp-machine` | `MVP/machine-p2panda` / `mvp-machine-p2panda` | Adapter depends on semantic crate and routing adapter. |
| Environment | `MVP/environment` / `mvp-environment` | `MVP/environment-p2panda` / `mvp-environment-p2panda` | Adapter depends on semantic crate; E2E depends on both. |
| Routing | `MVP/routing` / `mvp-routing` | `MVP/routing-p2panda` / `mvp-routing-p2panda` | Adapter depends on semantic crate; many crates use semantic routing directly. |

## Success Criteria

- The old pure-only implementations inside `MVP/deploy/`, `MVP/machine/`,
  `MVP/environment/`, and `MVP/routing/` are replaced by the p2panda-backed
  canonical implementations while the final crate identities remain
  unsuffixed.
- `commands-p2panda`, `deploy-p2panda`, `machine-p2panda`,
  `environment-p2panda`, and `routing-p2panda` suffixes are gone from crate
  directories, package names, library names, imports, and workspace members.
- Each command has one canonical implementation crate. The p2panda-backed
  implementation becomes canonical.
- No production Rust file under `MVP/` is above 1,500 LOC, except E2E test
  harnesses/contracts when intentionally scoped as tests. This is a guardrail,
  not the sole success condition.
- Every module reduction has a named missing concept recorded in this note and
  reflected in the resulting module/crate names.
- Remaining large or suspicious modules are audited for concept cohesion even
  when they are already below 1,500 LOC.
- `cargo test --manifest-path MVP/Cargo.toml --workspace` is green, or any
  narrower temporary verification is documented before the final full run.
- Existing product hardening changes in this worktree are preserved; this
  consolidation does not revert the daemon/status/serving-generation work.

## Non-Goals

- No Docker runtime work.
- No real WireGuard work.
- No new product primitives or behavior slices.
- No compatibility shims for old crate names unless needed for a short,
  intra-branch migration step that is removed before completion.
- No line-count-only shuffles. File moves are acceptable only when they follow
  a named concept boundary.

## Technical Decisions

### Canonical crate promotion is a move-plus-merge, not a delete-only step

The `-p2panda` crates are currently adapter crates that import the unsuffixed
semantic crates. Deleting the unsuffixed crates first would remove the domain
types and command state machines. The correct direction is:

1. Move each unsuffixed crate's domain modules into the p2panda-backed crate.
2. Move p2panda adapter code into a backend/writer module inside that canonical
   crate.
3. Rename the crate directory/package/lib back to the canonical unsuffixed name
   without leaving a `-p2panda` sibling.
4. Update callers to depend on the canonical crate only.
5. Delete the old directory and suffix references.

### Backend is an implementation detail, not a crate suffix

Crate names should describe product concepts: deploy, machine, environment,
routing. p2panda is the fact substrate backend. After consolidation it should
appear in module names only where the backend is explicitly configured or
tested, not in package names.

### Large files are reduced by domain ownership

Each oversized file gets one missing concept and an owner boundary. The target
is smaller files because concepts are clearer, not because files are chopped
arbitrarily.

### Implementing slices choose the abstraction

This note names the pressure and the concept to investigate. It does not lock
in the final abstraction, trait shape, crate split, or file layout. Each
implementation slice must deep dive the code, compare the idiomatic options,
and choose the smallest change that makes the code more readable. It is valid
to decide that simplification or better ownership inside the current module is
better than adding a new abstraction.

### Cohesive-as-is needs evidence

For especially large substrate files, the audit may not handwave that the file
is fine because it is conceptually cohesive. The audit must either produce a
named missing concept and implement the split, or write a concrete paragraph
that maps what the file does and explains why that growth represents one real
concept. This applies first to `MVP/p2panda-facts/src/lib.rs` and
`MVP/p2panda-authz/src/lib.rs`, which grew several times from their initial
size and are likely to contain accreted concerns.

## Missing Concepts By File

This section deliberately names the missing concept and the acceptance bar only.
It does not prescribe module names, file layouts, traits, or extraction
mechanics. The implementing agent for each reduction slice must deep dive the
current code, compare idiomatic options, and choose the smallest design that
satisfies the concept without over-abstracting.

### `MVP/bus/src/memory.rs`

Missing concept: **bus semantic state vs delivery execution runtime**.

The file currently owns subscription matching, request/reply delivery, queue
selection, service accounting, drain, and state mutation in one place.

Acceptance: the final design makes state semantics independently reviewable
from delivery execution, keeps bus behavior unchanged, and leaves no
non-E2E production file over 1,500 LOC.

### `MVP/p2panda-facts/src/lib.rs`

Missing concept: **fact substrate contract vs p2panda backend adapter**.

The file mixes Ployz fact metadata, p2panda operation encoding, SQLite store
open/rebuild, authorization, sync/import outcomes, and projection-facing source
methods.

Acceptance: substrate callers can be understood in Ployz fact terms before
dropping into raw p2panda details, tests still cover memory and durable stores,
and no non-E2E production file exceeds 1,500 LOC.

Audit rule: the next audit must produce either a named missing concept and a
split, or a concrete cohesive-as-is argument that maps the file's
responsibilities and explains why the growth is one real fact-substrate
concept rather than accreted backend, sync, store, and authorization concerns.

### `MVP/p2panda-authz/src/lib.rs`

Missing concept: **durable island membership model**.

The file combines signed membership operation envelopes, p2panda-auth group
processing, authority snapshots, durable replay, and tests.

Acceptance: `p2panda-authz` exposes an island membership API; p2panda-auth is
an implementation detail behind a clear authority boundary, and no non-E2E
production file exceeds 1,500 LOC.

Audit rule: the next audit must produce either a named missing concept and a
split, or a concrete cohesive-as-is argument that maps the file's
responsibilities and explains why the growth is one real durable-membership
concept rather than accreted envelope, reducer, authority-view, and store
concerns.

Implemented boundary:

- `MVP/p2panda-authz/src/identity.rs` owns island membership identifiers,
  author keys, member bindings, roles, and hashing helpers.
- `MVP/p2panda-authz/src/store.rs` owns durable membership log/store behavior
  for the in-memory and SQLite-backed operation logs.
- `MVP/p2panda-authz/src/lib.rs` now keeps the authority state machine,
  operation validation, access view, and public error/API surface under the
  threshold.

### `MVP/projection/src/reducer.rs`

Missing concept: **domain reducer contract**.

The reducer currently classifies and reduces node, service, serving, lease,
ACME, machine, and environment facts in one file.

Acceptance: top-level `reduce_facts` remains the public seam, but each domain
owns its key expectations, payload validation, and state mutation in the final
design, with no non-E2E production file over 1,500 LOC.

### `MVP/lease/src/lib.rs`

Missing concept: **lease ledger vs lease command policy**.

The file combines data carriers, command construction, reducer/selection,
clock/expiry policy, and bus writer behavior.

Acceptance: lease business rules are testable without the command/writer
surface, ACME dependencies are explicit, and no non-E2E production file exceeds
1,500 LOC.

### `MVP/acme/src/lib.rs`

Missing concept: **ACME challenge presentation domain**.

The file is currently under the hard threshold but is named in the goal.

Acceptance: keep the crate under 1,500 LOC and avoid introducing another ACME
god module while lease/projection are being reduced. If it must be changed, the
implementing agent should first identify the smallest ACME concept boundary
that falls out of the current code.

### `MVP/machine/src/remove.rs`

Missing concept: **machine remove operation state machine**.

The file combines validation, participant requests, serving cutover, recovery,
cleanup, and result assembly.

Acceptance: the canonical machine crate keeps the public remove command API,
the operation phases have clear ownership in the final design, and no non-E2E
production file exceeds 1,500 LOC.

### `MVP/iroh/src/facts.rs`

Missing concept: **legacy direct-iroh fact wrapper vs canonical p2panda transport**.

This file is not named in the prompt but is currently over threshold. It should
be investigated after crate canonicalization to decide whether the direct-iroh
fact wrapper remains part of the product path.

Acceptance: either the legacy path is removed with evidence that p2panda is
canonical, or the remaining iroh fact concept is explicit and below 1,500 LOC.

### `MVP/node/src/membership.rs`

Missing concept: **daemon runtime composition vs membership operations**.

This file grew during daemon hardening and now owns invite/admission, fact-node
spawn, peer admission application, node join publication, remote bridge refresh,
and daemon control.

Acceptance: daemon loop remains legible as runtime composition; membership
authority and local control are independently testable; no non-E2E production
file exceeds 1,500 LOC.

## Implementation Slices

### Slice Status

| Slice | Status | Evidence |
| --- | --- | --- |
| Slice 1: Baseline and design gates | Complete | This design note records the current duplicate crate graph, oversized files, missing concepts, sequencing, and verification checklist. |
| Slice 2: Promote routing to canonical | Complete | `PandaServingFactWriter` moved into `MVP/routing`; `MVP/routing-p2panda` removed from the workspace; `rg "mvp-routing-p2panda|mvp_routing_p2panda|routing-p2panda" MVP -g 'Cargo.toml' -g '*.rs'` returns no matches; `cargo test --manifest-path MVP/Cargo.toml -p mvp-routing` and `cargo test --manifest-path MVP/Cargo.toml -p mvp-node` pass. |
| Slice 3: Promote deploy to canonical | Complete | `PandaDeployFactWriter` moved into `MVP/deploy`; `MVP/deploy-p2panda` removed from the workspace; `rg "mvp-deploy-p2panda|mvp_deploy_p2panda|deploy-p2panda" MVP -g 'Cargo.toml' -g '*.rs'` returns no matches; `cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy`, `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`, and `MVP/scripts/three-server-smoke.sh` pass. |
| Slice 4: Promote environment to canonical | Complete | `PandaEnvironmentFactWriter` is exported from `MVP/environment`; `MVP/environment-p2panda` is absent from the workspace and e2e dependencies; `cargo test --manifest-path MVP/Cargo.toml -p mvp-environment` and `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- environment-branch-promote-rollback-contract` pass. |
| Slice 5: Promote machine to canonical | Complete | `PandaMachineFactStore` and `PandaMachineFactWriter` are exported from `MVP/machine`; `MVP/machine-p2panda` is absent from the workspace and e2e dependencies; `cargo test --manifest-path MVP/Cargo.toml -p mvp-machine` and `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract` pass. |
| Slice 6: Workspace suffix cleanup | Complete | `find MVP -maxdepth 1 -type d -name '*-p2panda'` returns no command adapter directories; `rg "mvp-(commands\|deploy\|machine\|environment\|routing)-p2panda\|commands-p2panda\|deploy-p2panda\|machine-p2panda\|environment-p2panda\|routing-p2panda\|mvp_(commands\|deploy\|machine\|environment\|routing)_p2panda" MVP -g 'Cargo.toml' -g '*.rs'` returns no matches; `cargo metadata --manifest-path MVP/Cargo.toml --no-deps --format-version 1` lists only canonical command crates plus p2panda substrate crates. |
| Slice 7: Fact substrate and authz concept extraction | Partial | `MVP/p2panda-facts/src/lib.rs` now keeps substrate contracts and data carriers while store/open/sync/shared-store runtime lives in `MVP/p2panda-facts/src/store_runtime.rs`; `MVP/p2panda-authz/src/lib.rs` now keeps membership/authz domain logic while identity types and durable store adapters live in `identity.rs` and `store.rs`; `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts -p mvp-p2panda-authz` passes. The next audit must still produce a named missing concept or a concrete cohesive-as-is argument for both files. |
| Slice 8: Bus semantic state extraction | Partial | Delivery execution runtime moved to `MVP/bus/src/memory/delivery_runtime.rs`, separating worker queue/inflight execution from semantic bus state; colocated tests were moved under semantically named `MVP/bus/src/memory/tests/` files; `cargo test --manifest-path MVP/Cargo.toml -p mvp-bus` passes. The remaining memory-state surface still needs concept-cohesion review. |
| Slice 9: Projection domain reducers | Pending | Test extraction reduced physical file size, but the reducer still needs a real domain-ownership audit. Do not declare this complete just because the file is below 1,500 LOC. |
| Slice 10: Lease, machine remove, daemon membership, iroh, and sub-threshold god-module audit | Pending | Test extraction lowered several files, but the named missing concepts remain unreviewed at the current code shape. The next slice must inspect these modules plus other suspicious files below 1,500 LOC. |
| Slice 11: Final consolidation gate | Pending | The suffix crate cleanup is implemented, but the final gate must wait for the concept-cohesion audit and full verification. |

### Slice 1: Baseline and design gates

Goal: make the current consolidation target explicit before code churn.

Work:

- Keep this note as the design gate in `MVP/design-notes/`.
- Add a small repository-local LOC/check script or `just` recipe if one exists
  nearby; otherwise use the audit commands above in the final verification.
- Record current duplicate crate references before moving code.

Tests:

- No behavior tests required beyond compile checks if only documentation and
  scripts are added.

### Slice 2: Promote routing to canonical

Goal: remove the first `-p2panda` crate suffix at the lowest dependency level.

Work:

- Move `MVP/routing/src/lib.rs` domain code into the canonical routing crate
  that also contains the p2panda writer backend.
- Keep package/lib names as `mvp-routing` / `mvp_routing`.
- Delete `MVP/routing-p2panda/`.
- Update callers from `mvp_routing_p2panda::PandaServingFactWriter` to the
  canonical routing crate.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-routing`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- E2E scenarios using serving writes: deploy, machine remove, environment.

### Slice 3: Promote deploy to canonical

Goal: make deploy one crate with p2panda fact persistence as the canonical
backend.

Work:

- Move `PandaDeployFactWriter` into `MVP/deploy/src/p2panda.rs`.
- Remove `MVP/deploy-p2panda/`.
- Update node/E2E imports to use `mvp_deploy::PandaDeployFactWriter`.
- Keep deploy domain/state-machine/fact modules in the canonical crate.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `MVP/scripts/three-server-smoke.sh`

### Slice 4: Promote environment to canonical

Goal: make environment branch/promote/rollback one crate with canonical
p2panda-backed fact writing.

Work:

- Move `PandaEnvironmentFactWriter` into the canonical environment crate.
- Delete `MVP/environment-p2panda/`.
- Update environment E2E and crate dependencies.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-environment`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- environment-branch-promote-rollback-contract`

### Slice 5: Promote machine to canonical

Goal: make machine remove one crate and prepare its large remove module for
concept extraction.

Work:

- Move `PandaMachineFactStore` and `PandaMachineFactWriter` into canonical
  machine modules.
- Delete `MVP/machine-p2panda/`.
- Update E2E and dependencies to use `mvp_machine::*`.
- Do not refactor `remove.rs` in the same edit except for imports required by
  the promotion.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-machine`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract`

### Slice 6: Command envelope suffix cleanup

Goal: remove the remaining `commands-p2panda` suffix so command envelopes have
one canonical persistence-aware surface.

Work:

- Move p2panda command envelope helpers into `MVP/commands`.
- Delete `MVP/commands-p2panda/`.
- Update E2E and callers to use `mvp_commands`.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-commands`
- Command-envelope E2E paths that currently import `mvp_commands_p2panda`.

### Slice 7: Workspace suffix cleanup

Goal: prove there are no parallel crates or `-p2panda` package names left.

Work:

- Remove suffix entries from `MVP/Cargo.toml`.
- Update `MVP/Cargo.lock`.
- Search and remove all remaining `deploy-p2panda`, `machine-p2panda`,
  `environment-p2panda`, `routing-p2panda`, `mvp_deploy_p2panda`,
  `mvp_machine_p2panda`, `mvp_environment_p2panda`, and
  `mvp_routing_p2panda` references.

Tests:

- `cargo metadata --manifest-path MVP/Cargo.toml`
- `cargo test --manifest-path MVP/Cargo.toml --workspace`

### Slice 8: Fact substrate and authz concept extraction

Goal: reduce the largest substrate files first by making p2panda backend
concepts explicit.

Work:

- Deep dive `MVP/p2panda-facts/src/lib.rs` and choose the smallest idiomatic
  shape that separates fact substrate contract from p2panda backend adapter.
- Deep dive `MVP/p2panda-authz/src/lib.rs` and choose the smallest idiomatic
  shape that separates durable island membership from backend reducer/store
  mechanics.
- Record the chosen boundaries before editing.
- Preserve public API unless callers are updated in the same slice.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-authz`
- p2panda fact/auth E2E contracts that use persistent stores and membership.

### Slice 9: Bus semantic state extraction

Goal: reduce the in-memory bus god module by separating state transition logic
from runtime delivery.

Work:

- Deep dive `MVP/bus/src/memory.rs`, choose the smallest idiomatic boundary
  between bus semantic state and delivery execution runtime, and record it
  before editing.
- Keep the public bus actor and harness APIs stable.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-bus`
- Any E2E that exercises bus request/reply and queue semantics.

### Slice 10: Projection domain reducers

Goal: reduce reducer size by making each domain own its reduction rules.

Work:

- Deep dive `MVP/projection/src/reducer.rs`, choose the smallest idiomatic
  reducer boundary that gives domains ownership of their rules, and record it
  before editing.
- Keep `reduce_facts` as the public function.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-projection`
- Projection-backed E2E contracts for serving, ACME, machine remove, and
  environment.

### Slice 11: Lease, machine remove, daemon membership, and iroh cleanup

Goal: bring remaining production files under the threshold.

Work:

- Deep dive `MVP/lease/src/lib.rs`, `MVP/machine/src/remove.rs`,
  `MVP/node/src/membership.rs`, and `MVP/iroh/src/facts.rs` before choosing
  extraction mechanics.
- Record the chosen boundary for each file before editing.
- For `MVP/iroh/src/facts.rs`, first decide whether old direct-iroh fact
  wrapper paths are still part of the canonical product path.
- Keep ACME under threshold; only split it if consolidation work increases it.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-lease`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-machine`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- Relevant ACME/machine/remove/process-serving E2E contracts.

### Slice 12: Final consolidation gate

Goal: prove acceptance.

Work:

- Run the LOC audit; no production files over 1,500 except E2E harnesses.
- Run suffix search; no `-p2panda` crate/package/import references remain.
- Run duplicate crate search; only canonical unsuffixed command directories
  remain, with no parallel suffix directories.
- Update this note from `proposed` to `implemented` with final evidence.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml --workspace`
- `MVP/scripts/three-server-smoke.sh`
- Any scenario-specific E2E required by changed domains.

## Sequencing Rationale

Promote routing first because deploy, machine, and environment all depend on
serving fact writing. Promote command crates next, one domain at a time, so
tests identify which command broke. Only after duplicate crates are gone should
large-file extraction begin; otherwise extraction has to be repeated across
old semantic crates and new adapter crates.

Substrate files are reduced before projection/business reducers because
p2panda-facts and p2panda-authz are the largest and most likely to affect
imports across every command. Projection and command refactors come after the
canonical crate graph is stable.

## Verification Checklist

- [ ] `MVP/commands-p2panda/`, `MVP/deploy-p2panda/`,
  `MVP/machine-p2panda/`, `MVP/environment-p2panda/`, and
  `MVP/routing-p2panda/` do not exist.
- [ ] `MVP/Cargo.toml` has no `*-p2panda` workspace members.
- [ ] `rg "-p2panda|_p2panda" MVP -g 'Cargo.toml' -g '*.rs'` returns no crate
  suffix references.
- [ ] `find MVP -path '*/src/*.rs' -not -path '*/e2e/*' -print0 | xargs -0 wc -l`
  shows no production files over 1,500 LOC.
- [ ] `cargo test --manifest-path MVP/Cargo.toml --workspace` passes.
- [ ] `MVP/scripts/three-server-smoke.sh` passes.
