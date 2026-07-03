---
artifact_contract: ce-unified-plan/v1
artifact_readiness: implementation-ready
date: 2026-07-03
execution: code
origin: architecture review candidates 1 and 4 (grilled 2026-07-03)
product_contract_source: ce-plan-bootstrap
title: "Managed Container Identity Consolidation - Plan"
type: refactor
---

# Managed Container Identity Consolidation - Plan

## Goal Capsule

- **Objective:** Make `ManagedContainerIdentity` the single owner of the
  container identity six-tuple (namespace, service, namespace revision entry,
  operation, step, kind), dissolve the structs that duplicate it flat, and
  give tests builders so identity-shape changes stop being 20-file sweeps.
- **Authority:** `CONTEXT.md` (Managed Container Identity, Container
  Provenance), ADR 0022, ADR 0023, AGENTS.md house rules.
- **Execution profile:** Behavior-preserving refactor plus one deliberate
  wire-shape change (nested `identity` object). No matching, planning, or
  gateway behavior changes.
- **Stop conditions:** Stop if this grows into changing what identity
  *means* (fields covered by the ADR 0022 digest), query-view reshaping, or
  server-derived namespace revision ids (recorded below as follow-up).
- **Decisions locked by grilling:** one struct not two (provenance is a named
  concept, not a type); nested JSON, not wire twins (`deny_unknown_fields`
  is incompatible with `serde(flatten)`); maximum dissolution; full test
  sweep with chained builders.

## Requirements

- R1. `ManagedContainerIdentity` lives in `ployz-core` with the six fields,
  `serde(deny_unknown_fields)`, TS export, and subset-matching methods
  (`is_running_service_entry`-style logic moves onto it) so planner and
  gateway matching share one implementation.
- R2. `ManagedContainerLabels` dissolves: the Docker label codec in
  `ployzd/src/docker/labels.rs` becomes `render(&ManagedContainerIdentity)`
  / `parse(labels) -> ManagedContainerIdentity` over the same `plz.*` label
  constants. Labels stay flat strings (they are not JSON).
- R3. `MachineContainerRunSpec` dissolves: the machine run RPC carries
  `container: ManagedContainerIdentity`. The RPC JSON is byte-identical
  before and after (the field already serializes exactly these six fields).
- R4. `ManagedContainerObservation` becomes `{ machine_id, container_id,
  identity, state }`; `DeployCleanupContainer` becomes `{ machine_id,
  container_id, identity }`. Both are deliberate wire-shape changes -
  greenfield, no aliases, no deploy consumer exists yet (Cloud integrates
  bootstrap only).
- R5. Query views (`RuntimeServiceInstance`, `RuntimeServiceRevision`,
  `GatewayServingEntry`, ...) are untouched: views project, they do not embed.
- R6. `ployz-test-support` gains chained `#[must_use]` builders:
  an identity builder, an observation builder with defaults
  (`default` namespace, test provenance) and state finishers
  (`.running_at(ip)`, `.exited()`), a snapshot helper, and extensions to the
  existing `fixtures.rs` deploy helpers.
- R7. Full sweep: all hand-built observation/labels/run-spec literals in
  integration tests (~33 sites) convert to builders; duplicated local
  `namespace_id`/`service_id`/... helper fns in integration tests are
  deleted in favor of `ployz_test_support::ids`. In-crate `#[cfg(test)]`
  unit modules keep their local helpers.
- R8. Regenerate `packages/ployz-sdk/src/generated.ts` and the operation
  contract fixture; SDK typecheck passes.

## Implementation Units

### U1. `ManagedContainerIdentity` in ployz-core

- **Files:** `crates/ployz-core/src/machine_runtime.rs` (or sibling module),
  `crates/ployz-sdk-types/src/lib.rs`, `typescript.rs`, exports test.
- Move the struct up from `ployzd/src/docker/labels.rs`, add matching
  methods; `ManagedContainerObservation::is_running_service_entry` and
  gateway `for_container` logic delegate to it.

### U2. Dissolve labels struct into the codec (R2)

- **Files:** `ployzd/src/docker/labels.rs`, `docker/runner.rs`,
  `machine_runtime/{runner,service,process}.rs`, `deploy_worker.rs`
  (`cleanup_expected_identity`, `retained_container_identity` now construct
  the core type), label tests.

### U3. Dissolve run spec (R3)

- **Files:** `ployzd/src/machine_runtime/protocol.rs`, `deploy_worker.rs`,
  `machine_runtime/service.rs`, machine RPC tests. Add a wire-pin test that
  the run request JSON is unchanged.

### U4. Embed identity in observation and cleanup (R4)

- **Files:** `ployz-core/src/machine_runtime.rs`, `deploy.rs` (planner
  filters via identity methods), `ployzd` gateway/facts/preparation/queries
  consumers, `ployz-nats` observation stores, wire-contract tests,
  regenerate TS + fixture (R8).

### U5. Builders and the sweep (R6, R7)

- **Files:** `ployz-test-support/src/{ids,fixtures}.rs` (+ new builder
  module), then the integration test files across `ployzd`, `ployz-nats`,
  `ployzctl`, `ployz-e2e` currently holding literals/helper clones.
- Builders land first, then U4's mechanical fallout is absorbed by
  converting each touched site to builders in the same pass (one blast
  radius, one pass).

## Deferred follow-up (recorded, not in scope)

- Server-derived `namespace_revision_id`: since entry identities took over
  all matching, the caller-supplied revision id is only an attempt label,
  and `CONTEXT.md` already says Ployz derives it. Deriving it core-side
  would shrink the Cloud deploy contract to operation id + namespace +
  services. Own plan, own grilling.

## Verification

| Gate | Done signal |
|---|---|
| `cargo test -p ployz-core` | Identity methods, planner, wire contract (new nested shapes pinned). |
| `cargo test -p ployzd` | Labels codec round-trip, machine RPC wire-pin (unchanged JSON), gateway/preparation suites. |
| `cargo test -p ployz-nats -p ployzctl -p ployz-sdk-types` | Stores, CLI contract, TS contract. |
| SDK `npm run typecheck` after regeneration | Nested identity type flows through generated.ts. |
| `cargo clippy --all-targets` | No new warnings. |
| Grep gate | Zero remaining flattened six-tuple struct literals outside query views; zero local id-helper fns in integration tests. |
