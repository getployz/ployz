---
title: MVP Consolidation Before New Product Slices
status: active
created: 2026-05-20
type: refactor
origin: user consolidation goal
design_note: MVP/design-notes/2026-05-20-consolidation-plan.md
---

# MVP Consolidation Before New Product Slices

## Problem Frame

MVP product work is blocked by two forms of structural debt:

- Duplicate command crates still exist after the p2panda-backed replacements
  shipped. Callers can still depend on both a pure semantic crate and a
  `-p2panda` adapter crate.
- Several files are large because they own multiple concepts at once. Reducing
  them by moving lines around would preserve the debt; each reduction needs a
  named abstraction or ownership boundary.

This plan stops new product feature work until the command crate graph is
canonical and the oversized modules have concept-backed boundaries. It follows
`VISION.md`: primitive operations stay explicit, backend details stay below
the command surface, and the daemon remains a composition/runtime owner rather
than the place feature state accumulates.

The design gate for the missing abstractions is
`MVP/design-notes/2026-05-20-consolidation-plan.md`.

## Scope

In scope:

- Promote p2panda-backed command implementations into canonical unsuffixed
  crates.
- Delete parallel `-p2panda` command crates and update callers.
- Include `MVP/commands-p2panda/` in the suffix cleanup because the acceptance
  criterion is no `-p2panda` suffixes, even though the initial prompt listed
  deploy, machine, environment, and routing.
- Treat the 1,500 LOC threshold as a smoke alarm and final guardrail, not as
  the target. The real work is finding modules whose concepts are muddled,
  even when they are already under the threshold.
- Keep archival slice documents intact unless they are active docs that would
  mislead implementers.

Out of scope:

- Docker runtime replacement.
- Real WireGuard wiring.
- New product primitives, status commands, daemon readiness features, gateway
  behavior changes, or serving/deploy feature hardening.
- Compatibility shims for old crate names after the consolidation lands.

## Requirements

- R1: There is one canonical implementation per command domain.
- R2: No active workspace member, package name, library name, or Rust import
  uses `commands-p2panda`, `deploy-p2panda`, `machine-p2panda`,
  `environment-p2panda`, or `routing-p2panda`.
- R3: The p2panda-backed implementation becomes the canonical unsuffixed
  command implementation.
- R4: No production Rust file under `MVP/` is over 1,500 physical LOC except
  intentionally scoped E2E harnesses/contracts, but passing that check is not
  enough by itself.
- R5: Every module reduction is justified by a named missing concept,
  simplification, crate boundary, or ownership boundary in
  `MVP/design-notes/2026-05-20-consolidation-plan.md`.
- R6: Reduction slices must not treat the design note as a prescribed file
  layout. The implementing agent must deep dive the current code and choose the
  most idiomatic minimal abstraction that satisfies the named concept without
  over-abstracting.
- R6a: Files below 1,500 LOC remain eligible for consolidation when they are
  unreadable, own too many responsibilities, or hide a missing domain concept.
- R7: Existing product hardening changes in the worktree are preserved; this
  consolidation must not revert unrelated daemon/status/serving work.
- R8: Final verification runs the workspace tests or records a concrete
  blocker with the exact failing target.

## Current Audit

Duplicate command crate pairs:

| Canonical domain | Current semantic crate | Current suffix crate |
| --- | --- | --- |
| Commands | `MVP/commands` / `mvp-commands` | `MVP/commands-p2panda` / `mvp-commands-p2panda` |
| Routing | `MVP/routing` / `mvp-routing` | `MVP/routing-p2panda` / `mvp-routing-p2panda` |
| Deploy | `MVP/deploy` / `mvp-deploy` | `MVP/deploy-p2panda` / `mvp-deploy-p2panda` |
| Environment | `MVP/environment` / `mvp-environment` | `MVP/environment-p2panda` / `mvp-environment-p2panda` |
| Machine | `MVP/machine` / `mvp-machine` | `MVP/machine-p2panda` / `mvp-machine-p2panda` |

Current oversized files from the audit:

| File | LOC | Plan |
| --- | ---: | --- |
| `MVP/p2panda-facts/src/lib.rs` | 4,784 | Separate fact substrate contract from p2panda backend adapter. |
| `MVP/bus/src/memory.rs` | 3,841 | Separate bus semantic state from delivery execution runtime. |
| `MVP/p2panda-authz/src/lib.rs` | 3,362 | Separate island membership model from p2panda-auth reducer/store adapters. |
| `MVP/projection/src/reducer.rs` | 2,796 | Introduce per-domain reducers behind one reducer contract. |
| `MVP/lease/src/lib.rs` | 1,863 | Separate lease ledger, policy, writer, and command surface. |
| `MVP/iroh/src/facts.rs` | 1,689 | Delete or shrink legacy direct-iroh fact wrapper paths. |
| `MVP/machine/src/remove.rs` | 1,658 | Separate remove state machine, participants, recovery, planner, and executor. |
| `MVP/node/src/membership.rs` | 1,634 | Separate daemon runtime composition from membership operations. |

`MVP/acme/src/lib.rs` is currently under 1,500 LOC, but the design note keeps
it on the watchlist because it was explicitly named in the goal.

## Decisions

### Promote, do not preserve adapter siblings

The current `-p2panda` crates are adapter crates that still depend on the
unsuffixed semantic crates. The final shape should keep the unsuffixed crate
identity and move the p2panda-backed writer/store modules into it. The old
pure-only implementation contents are replaced; the final crate name remains
canonical.

### Backend names belong below the command surface

`p2panda` can remain in backend module names where it is real implementation
detail. It should not appear as a command-domain crate suffix. Product-level
crate names should stay `commands`, `routing`, `deploy`, `environment`, and
`machine`.

### Remove duplicate crates before god-module refactors

If large-file extraction happens first, implementation work has to thread
through both semantic crates and adapter crates. Canonicalizing the crate graph
first makes later abstractions land once.

### Treat 1,500 lines as a smoke alarm, not a destination

The threshold catches obvious pain, but the consolidation goal is serious
product shape: readable modules with crisp owners. A file can be too muddled
below 1,500 lines, and a reduction can still be bad if it merely creates more
files without clarifying a concept.

### Treat test bloat separately from production god modules

Some threshold offenders are mostly large because tests live beside production
code. Moving tests is allowed only when the production concept is already clear
or the file is otherwise over the physical threshold. It should not be used as
the only explanation for a production module that lacks an owner boundary.
Test module names should describe behavior areas, not mechanical chunks.

### Defer LOC extraction mechanics to implementation

The plan names missing concepts because that is the design gate. It does not
choose module names, trait shapes, crate splits, or extraction order inside a
file. Each reduction slice starts with a code deep dive and a short local
decision record before edits.

### Require evidence for cohesive-as-is findings

For `MVP/p2panda-facts/src/lib.rs` and `MVP/p2panda-authz/src/lib.rs`, the
audit must produce either a named missing concept and a split, or a concrete
cohesive-as-is paragraph that maps the file's responsibilities and explains why
the growth represents one real concept. These files grew several times from
their initial size, so "conceptually cohesive" is not an acceptable conclusion
without evidence.

## Implementation Slices

Current status:

- Slice 0 baseline/design gate is complete in
  `MVP/design-notes/2026-05-20-consolidation-plan.md`.
- Routing is canonical: `MVP/routing-p2panda/` is removed, callers use
  `mvp-routing`, and routing/node tests passed.
- Deploy is canonical: `MVP/deploy-p2panda/` is removed, callers use
  `mvp-deploy`, `cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy`,
  `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`, and
  `MVP/scripts/three-server-smoke.sh` passed.
- Commands, environment, and machine are canonical too: no top-level
  `*-p2panda` command adapter directories remain, suffix imports are absent,
  environment and machine e2e contracts passed, and metadata lists only
  canonical command crates plus p2panda substrate crates.
- Duplicate-crate consolidation is implemented. Large-file consolidation is
  partially implemented: p2panda facts/authz have named substrate, identity,
  and store-runtime modules, and bus delivery execution is separate from
  semantic memory state. The current LOC audit is no longer sufficient to call
  the god-module work complete; the next slice must audit concept cohesion
  across the remaining large and suspicious modules, including files already
  below 1,500 lines.

### Slice 0: Baseline and Design Gate

Goal: lock the consolidation target before moving code.

Work:

- Keep `MVP/design-notes/2026-05-20-consolidation-plan.md` as the design gate.
- Run the duplicate-crate and LOC audits from the design note.
- Record any existing test failures before refactoring so consolidation does
  not absorb unrelated hardening failures.
- Confirm active docs that must be updated: `MVP/README.md`,
  `MVP/architecture.md`, and `MVP/Cargo.toml`.

Verification:

- `cargo metadata --manifest-path MVP/Cargo.toml`
- The LOC audit command from the design note.

### Slice 1: Promote Routing and Commands

Goal: remove the lowest-level suffix crates first.

Work:

- Move `MVP/routing-p2panda/src/lib.rs` backend writer code into
  `MVP/routing/src/`.
- Delete `MVP/routing-p2panda/`.
- Move `MVP/commands-p2panda/src/lib.rs` persistence helpers into
  `MVP/commands/src/`.
- Delete `MVP/commands-p2panda/`.
- Update `MVP/e2e/Cargo.toml`, `MVP/node/Cargo.toml`, and any Rust imports to
  use `mvp-routing` and `mvp-commands`.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-routing`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-commands`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`

### Slice 2: Promote Deploy

Goal: make deploy one canonical p2panda-backed crate.

Work:

- Move `MVP/deploy-p2panda/src/lib.rs` writer/store code into
  `MVP/deploy/src/p2panda.rs` or a similarly named backend module.
- Delete `MVP/deploy-p2panda/`.
- Update `MVP/node`, `MVP/runtime`, and `MVP/e2e` dependencies/imports.
- Preserve deploy domain, facts, coordinator, and state-machine APIs unless
  imports require a local rename.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `MVP/scripts/three-server-smoke.sh`

### Slice 3: Promote Environment

Goal: make environment branch/promote/rollback one canonical crate.

Work:

- Move `MVP/environment-p2panda/src/lib.rs` backend code into
  `MVP/environment/src/`.
- Delete `MVP/environment-p2panda/`.
- Update E2E and workspace dependencies.
- Keep environment command/domain/fact behavior unchanged.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-environment`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- environment-branch-promote-rollback-contract`

### Slice 4: Promote Machine

Goal: make machine remove one canonical crate before refactoring its remove
operation internals.

Work:

- Move `MVP/machine-p2panda/src/lib.rs` fact writer/store code into
  `MVP/machine/src/`.
- Delete `MVP/machine-p2panda/`.
- Update dependencies/imports in `MVP/e2e`, `MVP/node`, and any command tests.
- Avoid restructuring `MVP/machine/src/remove.rs` in this slice except for
  import changes required by the promotion.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-machine`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract`

### Slice 5: Workspace Suffix Gate

Goal: prove canonical command crate identity before touching the god modules.

Work:

- Remove suffix members from `MVP/Cargo.toml`.
- Update `MVP/Cargo.lock`.
- Update active docs that describe command crates.
- Search and remove active code/Cargo references to `-p2panda` command suffixes
  and `_p2panda` command imports.

Tests:

- `cargo metadata --manifest-path MVP/Cargo.toml`
- `cargo test --manifest-path MVP/Cargo.toml --workspace`

Acceptance for this slice:

- `MVP/commands-p2panda/`, `MVP/routing-p2panda/`,
  `MVP/deploy-p2panda/`, `MVP/environment-p2panda/`, and
  `MVP/machine-p2panda/` do not exist.
- `MVP/Cargo.toml` has no command-domain `*-p2panda` members.

### Slice 6: Fact Substrate and Authz Boundaries

Goal: reduce the largest substrate files by making their ownership boundaries
explicit.

Work:

- Deep dive `MVP/p2panda-facts/src/lib.rs` and choose the smallest idiomatic
  shape that separates the fact substrate contract from p2panda backend
  details.
- Deep dive `MVP/p2panda-authz/src/lib.rs` and choose the smallest idiomatic
  shape that separates the durable island membership model from p2panda-auth
  reducer/store details.
- Record the chosen boundaries before editing. For each file, produce either a
  named missing concept and split, or a concrete cohesive-as-is argument that
  maps the file's responsibilities.
- Keep public APIs stable where practical; update internal imports in the same
  slice when the new modules expose clearer names.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-authz`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-auth-membership-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-sync-fact-source-contract`

### Slice 7: Bus Runtime Boundary

Goal: make the `MVP/bus/src/memory.rs` ownership boundary clear by separating
state transitions from delivery execution when that remains the best
abstraction after the deep dive.

Work:

- Deep dive `MVP/bus/src/memory.rs` and choose the smallest idiomatic shape
  that separates bus semantic state from delivery execution runtime.
- Record the chosen boundary before editing; the final shape must satisfy the
  threshold guardrail without treating it as the design target.
- Keep actor and contract-facing APIs stable.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-bus`
- Bus E2E contracts that cover request/reply and queue semantics.

### Slice 8: Projection Reducers

Goal: make projection domain ownership clear if the deep dive shows the
reducer is still unreadable or mixing responsibilities after test extraction.

Work:

- Deep dive `MVP/projection/src/reducer.rs` and choose the smallest idiomatic
  shape that makes domain reduction ownership explicit.
- Record the chosen reducer boundary before editing. The implementation may
  split files, introduce a trait, simplify data flow, or leave code in place
  if the deep dive shows no real concept boundary worth extracting.
- Keep `reduce_facts` as the public entry point.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-projection`
- Projection-backed E2E contracts for serving, ACME, machine remove, and
  environment promotion/rollback.

### Slice 9: Remaining God-Module Risks

Goal: audit the remaining god-module risk, not only the remaining threshold
offenders.

Work:

- Deep dive `MVP/lease/src/lib.rs`, `MVP/machine/src/remove.rs`,
  `MVP/node/src/membership.rs`, `MVP/iroh/src/facts.rs`,
  `MVP/acme/src/lib.rs`, and other suspicious production files before choosing
  extraction mechanics.
- For each file, record the chosen boundary against the missing concept in the
  design note, then either implement the reduction or explicitly defend why no
  split is the more idiomatic result.
- For `MVP/iroh/src/facts.rs`, first determine whether the direct-iroh fact
  path is still product-canonical after p2panda crate consolidation.
- Keep `MVP/acme/src/lib.rs` readable; split only if the audit finds a real
  ACME concept boundary, not just because it is on the watchlist.

Tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-lease`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-machine`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-iroh`
- ACME, machine-remove, and membership-backed E2E contracts affected by the
  moved modules.

### Slice 10: Final Consolidation Gate

Goal: prove the branch is ready for product work to resume.

Work:

- Run the suffix search and remove any remaining active references.
- Run the LOC audit and confirm no non-E2E production file exceeds 1,500 LOC.
- Run the concept-cohesion audit from the design note and record any remaining
  god-module risks explicitly instead of hiding behind the LOC number.
- Update `MVP/design-notes/2026-05-20-consolidation-plan.md` with final
  evidence and change its status if the work has landed.
- Update `MVP/README.md` and `MVP/architecture.md` if crate names or backend
  boundaries changed.

Tests:

- `cargo metadata --manifest-path MVP/Cargo.toml`
- `cargo test --manifest-path MVP/Cargo.toml --workspace`
- `MVP/scripts/three-server-smoke.sh`

## Acceptance Checks

Use these as the final branch gates:

```text
find MVP -path '*/target/*' -prune -o -type f -name Cargo.toml -print | xargs rg -n "commands-p2panda|deploy-p2panda|machine-p2panda|environment-p2panda|routing-p2panda"
rg -n "mvp_(commands|deploy|machine|environment|routing)_p2panda|mvp-(commands|deploy|machine|environment|routing)-p2panda" MVP -g '*.rs' -g 'Cargo.toml'
find MVP -path '*/target/*' -prune -o -path 'MVP/e2e/*' -prune -o -type f -name '*.rs' -print0 | xargs -0 wc -l | sort -nr | head -20
cargo test --manifest-path MVP/Cargo.toml --workspace
```

The first two commands should return no active crate/package/import suffix
references. The LOC command should show no non-E2E production Rust file above
1,500 lines, and the design note should explain why the remaining large
modules are conceptually acceptable or what consolidation slice remains.

## Risks

- The current worktree already contains unrelated hardening changes. Start with
  a baseline so consolidation failures are not mistaken for pre-existing
  failures.
- Some `-p2panda` mentions are historical slice docs. Do not rewrite history
  just to make `rg` quiet; restrict the final no-suffix gate to active Cargo,
  Rust, README, architecture, and current design notes.
- Moving p2panda-backed adapters into canonical crates may expose circular
  dependencies that were hidden by adapter crates. Resolve those by moving
  shared contracts downward, not by importing daemon/runtime convenience types
  upward.
