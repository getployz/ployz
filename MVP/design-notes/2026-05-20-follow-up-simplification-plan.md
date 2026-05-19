---
title: MVP Follow-Up Simplification Plan
status: active
created: 2026-05-20
scope: concept-deletion-after-consolidation
---

# MVP Follow-Up Simplification Plan

## Problem Frame

The duplicate-crate cull and god-module reduction got the MVP below the hard
line-count guardrail, but the final audit found remaining complexity that will
make the next product slice balloon again. The remaining work is not another
module reshuffle. It must reduce product-shaped surface area, delete ceremony,
or introduce a missing concept that prevents future accretion.

This plan keeps new product features stopped. No Docker runtime work, no real
WireGuard work, and no new primitive slices land until the hotspots below have
either been simplified or explicitly rejected as cohesive.

## Success Criteria

- Remaining simplification slices reduce LOC or public surface area, not just
  move code between files.
- Each implemented slice names the concept being deleted or introduced before
  code changes.
- Production paths stop depending on fixture/harness-shaped APIs where a
  product runtime concept exists.
- Fact substrate ownership becomes clearer: p2panda storage, derived indexes,
  projection-facing candidates, authority snapshots, and fixture/debug paths
  are not all one public concept.
- The command framework represents the durable phase primitive actually used
  by production code, without unused compensation ceremony.
- The daemon loop remains a composition root, but membership admission, fact
  sync, control socket, and node-agent RPC pumps are not hidden in one long
  operational loop.
- Projection fact-key schema knowledge has one typed contract or catalog, not
  parallel match blocks that expand independently.
- Verification stays green with targeted crate tests during slices and
  `cargo test --manifest-path MVP/Cargo.toml --workspace` before completion.

## Non-Goals

- No behavior shortcuts to pass current tests.
- No compatibility shims for deleted public test fixtures unless a real
  downstream caller is found in this workspace.
- No line-count-only splits.
- No attempt to replace p2panda, Docker, WireGuard, gateway, or DNS behavior.

## Findings From Final Audit

### Fact substrate owns too many concepts

`MVP/p2panda-facts/src/store_runtime.rs` still combines p2panda operation
construction, authorization checks, ingest/import, conflict classification,
derived index rebuild, shared locking, topic store adapters, and projection
`FactSource` output. `MVP/p2panda-facts/src/backend.rs` also keeps a memory
backend with product-shaped behavior beside SQLite-backed storage.

Missing concept: **fact store runtime vs derived fact index vs projection
candidate catalog**.

### Daemon runtime composition is still too dense

`MVP/node/src/membership.rs` now has daemon control extracted, but
`run_daemon_once` still performs setup, self-advertisement, peer admission,
fact-node spawning, stream refresh, bridge registration, and node-agent RPC
handling.

Missing concept: **daemon supervisor services**. The daemon loop should tick
named services rather than own each subsystem's operational details.

### Projection duplicates fact-key schema parsing

`MVP/projection/src/source.rs` classifies fact keys while
`MVP/projection/src/reducer/key_expectation.rs` separately parses the same key
shapes to validate payload identity. Adding a fact kind requires touching
parallel schema matches.

Missing concept: **typed fact key catalog** shared by classification and
payload expectation validation.

### Command framework is more general than production use

`MVP/commands/src/lib.rs` models generic compensation, while production
commands currently implement no-op compensation and still hand-roll phase
matches. The useful primitive is durable intent plus append-only phase history.

Missing concept: **durable phase log**, not a generic compensating workflow
framework.

### Bus runtime is exposed as harness

Production composition in `MVP/node/src/deploy.rs` and
`MVP/node/src/membership.rs` imports `mvp_bus::harness::InMemoryBus`. That name
keeps a test concept in product runtime wiring.

Missing concept: **local bus runtime** separate from test harness helpers.

### Fixture substrates remain product-shaped

`MVP/bus/src/facts.rs`, `MVP/projection/src/bus_source.rs`, and
`IslandAuthzMemoryLog` are useful fixtures, but their public names make old
proof paths look like viable product substrates.

Missing concept: **fixture/debug substrate**, clearly separate from canonical
p2panda-backed facts.

## Slice Plan

### Slice 1: Collapse phased-command compensation ceremony

Status: complete.

Goal: make `MVP/commands` express the durable phase log primitive that
production commands actually use.

Implementation:

- Remove `CommandCompensationFuture` and `PhasedCommand::compensate`.
- Make `run_phased` append phases and resume from stored history, but return
  the original step/write error directly without best-effort compensation.
- Delete compensation-only test harness state and replace tests with coverage
  for append failure, resume, conflict, history gap, and concurrent advance.
- Remove no-op compensation implementations from environment and machine
  commands.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-commands` passed.
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-environment` passed.
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-machine` passed.

Evidence:

- `MVP/commands/src/lib.rs`, `MVP/environment/src/command.rs`, and
  `MVP/machine/src/remove.rs` removed 193 lines and added 28 lines in this
  slice.
- `rg -n "CommandCompensationFuture|compensate\\(" MVP -g '*.rs'` returns no
  matches.

### Slice 2: Rename local bus runtime boundary

Status: complete.

Goal: stop production code from importing test-harness vocabulary.

Implementation:

- Add an explicit `mvp_bus::local` or `mvp_bus::runtime` constructor for
  `MemoryBus` plus `BusActorHandle`.
- Update production composition in `MVP/node` to use the local runtime name.
- Keep `mvp_bus::harness` for tests only where possible, or make it a thin
  wrapper over the local runtime.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-bus` passed.
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node` passed.

Evidence:

- `MVP/bus/src/lib.rs` now exposes `mvp_bus::local::LocalBus` and
  `mvp_bus::local::actor_with_authority()` for local runtime composition.
- `MVP/node/src/deploy.rs` and `MVP/node/src/membership.rs` now use the local
  runtime surface instead of `mvp_bus::harness`.
- `rg -n "mvp_bus::harness|harness::InMemoryBus|harness::actor_with_authority" MVP/node/src/deploy.rs MVP/node/src/membership.rs`
  returns no matches.

### Slice 3: Introduce typed fact key catalog

Status: complete.

Goal: stop projection classification and key-expectation validation from
growing parallel match blocks.

Implementation:

- Introduce a typed parsed fact key enum in `MVP/projection`.
- Derive `FactKind`/epoch classification and payload key expectations from
  that parsed representation.
- Keep reducer domain modules focused on payload semantics, not key grammar.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-projection` passed.
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- projection-contract`
  passed.

Evidence:

- `MVP/projection/src/source.rs` now owns `ParsedFactKey`; both fact-key
  classification and payload identity validation derive from that parsed key.
- `MVP/projection/src/reducer/key_expectation.rs` no longer parses key
  segments or carries a separate `KeyExpectation` enum.
- `MVP/projection/src/source.rs` and
  `MVP/projection/src/reducer/key_expectation.rs` removed 284 lines and added
  204 lines in this slice.

### Slice 4: Clarify p2panda fact ownership

Goal: make p2panda fact storage, derived indexes, and projection source output
separate concepts with smaller public surfaces.

Implementation:

- Move derived candidate indexing behind a named index type.
- Keep p2panda operation import/write paths in the store runtime.
- Move projection `FactSource` adapter logic to a projection-facing adapter
  module or type instead of making the store itself look like a reducer source.
- Quarantine manual trust/import helpers as fixture/debug APIs where tests
  still require them.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts`
- p2panda fact source, sync, process role, and auth membership E2Es.

### Slice 5: Split daemon loop by supervisor services

Goal: keep `run_daemon_once` as readable composition over named services.

Implementation:

- Identify daemon services by operational audience: control socket, fact sync,
  membership publication/admission, remote bridge registration, node-agent RPC.
- Extract service-owned tick/apply functions only where doing so removes
  repeated state plumbing or separates failure audiences.
- Keep startup order explicit in `run_daemon_once`.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-node`
- `MVP/scripts/three-server-smoke.sh`

### Slice 6: Fixture substrate quarantine

Goal: make old proof substrates hard to use accidentally in production paths.

Implementation:

- Rename public fixture surfaces or gate them behind test/e2e-facing modules
  where workspace callers allow it.
- Replace production-looking imports with p2panda-backed stores or explicit
  local runtime fixtures.
- Update design notes so deletion triggers are concept/caller based, not stale
  LOC based.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml --workspace`
- Search for production imports of fixture/harness-only surfaces.

## Final Gate

- No new product feature slice has landed during this simplification work.
- `cargo test --manifest-path MVP/Cargo.toml --workspace` passes.
- A final audit lists remaining hotspots and marks each as simplified,
  cohesive-as-is, or deferred with a concrete trigger.
