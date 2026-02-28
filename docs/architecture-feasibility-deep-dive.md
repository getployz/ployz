# Architecture Feasibility Deep Dive

This document evaluates the feasibility of evolving the current `ployz` repository toward the target package structure described in the planning note (`domain/`, `node/`, `store/`, `transport/`, `runtime/`, `health/`, `overlay/`, `api/`, `platform/`, `testing/`, `internal/`).

## Executive Assessment

**Feasibility:** High  
**Risk profile:** Moderate (mostly migration-sequencing risk, not conceptual risk)  
**Recommended approach:** Incremental convergence to the target boundaries, not a full tree rewrite.

The repository already follows most of the target principles:

- Thin binary entrypoints (`cmd/ployz`, `cmd/ployzd`)
- Core daemon capabilities separated from infra adapters
- Platform-specific wiring in dedicated platform packages
- Interface-driven orchestration layers

The main gap is **shape and naming consistency**, not foundational architecture.

## Current-State vs Target-State Mapping

| Target concept | Current equivalent | Feasibility | Notes |
|---|---|---:|---|
| `domain/` pure types | `internal/daemon/*` types + `pkg/sdk/types` | Medium | Domain types exist but are distributed across capability packages by design. A hard central `domain/` package is optional. |
| `node/` orchestration brain | `internal/daemon/manager` + `internal/daemon/convergence` | High | Existing manager+convergence split already expresses orchestration and reconciliation roles. |
| `store/` interfaces | `internal/daemon/*/ports.go` + `internal/infra/corrosion`/`sqlite` | High | Consumer-owned ports pattern is already in use. |
| `transport/` mesh interface | `internal/infra/wireguard` + overlay ports | Medium | Transport exists, but names are capability-centric instead of transport-centric. |
| `runtime/` workload execution | `internal/infra/docker` + `internal/daemon/workload` | High | Runtime abstraction exists and can be tightened. |
| `health/` subsystem | `internal/daemon/convergence` health/freshness/ntp/ping | High | Health logic already present; mostly packaging and API ownership choices. |
| `overlay/` boot lifecycle | `internal/daemon/overlay` | Very High | Already implemented directly. |
| `api/` surface | `internal/daemon/api` | Very High | Already implemented directly. |
| `platform/` concrete infra | `internal/infra/platform` + `internal/infra/*` | Very High | Already implemented directly. |
| `testing/` harness layers | mixed package-local tests + infra tests | Medium | Can be improved with explicit shared cluster/nemesis harnesses. |

## Hard Constraints and Non-Negotiables

The following constraints must remain intact during any migration:

1. **No reversal of dependency direction.** Core packages cannot import infra packages.
2. **Cross-platform behavior parity** must hold for `linux`, `darwin`, and stubs.
3. **CLI behavior compatibility** requires updated help/output if semantics change.
4. **Daemon remains single policy boundary** where API handlers delegate to manager/capabilities.
5. **Mandatory green gates**: `just test` and `just build` for every migration step.

## Exact Feasibility by Change Class

### 1) Pure package renames/moves

**Feasibility:** Medium  
**Difficulty:** Medium-High  
**Why:** Go import path churn is mechanical but wide-reaching; risk is accidental API exposure and test breakage.

- Cost drivers:
  - Update imports across daemon, SDK, CLI, and tests
  - Resolve circular dependencies exposed by path changes
  - Preserve build tags and platform file pairing

**Recommendation:** Avoid large renames until behavior and boundaries are already stabilized.

### 2) Boundary tightening without renames

**Feasibility:** Very High  
**Difficulty:** Medium  
**Why:** Existing architecture already supports this path.

- Actions:
  - Move remaining side effects behind existing ports
  - Keep manager as policy orchestrator
  - Add typed subscription APIs where raw data flows leak

**Recommendation:** Make this the default migration strategy.

### 3) Introducing centralized `domain/`

**Feasibility:** Conditional  
**Difficulty:** Medium  
**Why:** Current repo intentionally uses capability-owned types. A single `domain/` package may conflict with that rule if over-applied.

- Good candidates for centralization:
  - Shared IDs/value objects with no behavior
  - Cross-cutting enum-like statuses
- Bad candidates:
  - Capability-specific persistence models
  - Types that force unnecessary imports into unrelated packages

**Recommendation:** Adopt a **minimal domain-core** only when two or more capabilities need truly shared, infrastructure-agnostic value types.

### 4) Convergence/scheduler decomposition

**Feasibility:** High  
**Difficulty:** Medium  
**Why:** Convergence loops already exist and can absorb a pure planning/scheduling package.

- Preconditions:
  - Keep scheduler pure (input state -> placement plan)
  - Keep application of plan in convergence/orchestration layers

**Recommendation:** Safe and beneficial, especially for deterministic tests.

### 5) Dedicated integration test harness (`testing/cluster`, `testing/nemesis`)

**Feasibility:** High  
**Difficulty:** Medium  
**Why:** Existing tests can be incrementally wrapped with reusable harness utilities.

- Benefits:
  - Repeatable partition/heal scenarios
  - Better confidence for distributed correctness

**Recommendation:** Build this early if cluster behavior is evolving quickly.

## Estimated Effort (Exact Bands)

Assuming one experienced contributor and existing CI:

- **Phase A: Boundary audit + adapter cleanup:** 3-5 engineering days
- **Phase B: Test harness improvements (success/failure paths):** 4-7 engineering days
- **Phase C: Optional selective package shape convergence:** 3-6 engineering days
- **Phase D: Optional large-scale renames/re-home:** 5-10 engineering days

### Total by strategy

- **Pragmatic (recommended):** 2-4 weeks
- **Full shape rewrite:** 4-8 weeks

## Critical Risks and Mitigations

### Risk 1: Hidden infra coupling in core loops

- **Symptom:** core package imports drift toward concrete adapters
- **Mitigation:** add lint/check review rule: no `internal/infra` imports in core capability dirs

### Risk 2: Cross-platform drift during refactors

- **Symptom:** linux works, darwin/stub silently regress
- **Mitigation:** require platform build matrix on every migration PR; keep build-tag file sets paired

### Risk 3: Behavior drift from “pure refactor” steps

- **Symptom:** CLI/daemon output changes unexpectedly
- **Mitigation:** treat UX outputs as API; update command help text and snapshot-like tests where applicable

### Risk 4: Migration stalls under broad scope

- **Symptom:** long-lived branch with mounting conflicts
- **Mitigation:** vertical slices merged continuously behind stable interfaces

## Recommended Migration Sequence (Low-Regret Path)

1. **Baseline inventory:** map each core package to owned ports and side effects.
2. **Seal boundaries:** remove direct adapter calls from core logic where found.
3. **Add missing failure-path tests:** especially in overlay/convergence/membership changes.
4. **Extract pure planners/schedulers:** only where logic is already naturally pure.
5. **Introduce shared test harness primitives:** partition/heal/lag scenarios.
6. **Optional package naming convergence:** only after logic and tests stabilize.

## Feasibility Verdict

You can reach the target architecture with high confidence **without** a disruptive rewrite.

The most feasible route is to preserve current package ownership principles and converge behaviorally toward the desired shape. In this codebase, the architecture is already close; the remaining work is mostly boundary rigor, naming alignment, and stronger distributed-system test harnesses.

## Follow-up TODOs

- Add a migration checklist doc that maps concrete files to each phase and owner.
- Add guardrail checks for forbidden import directions in CI.
- Add explicit scenario tests for partition/heal/stale-writer recovery.
