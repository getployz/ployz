---
title: "feat: Add compute-only region placement"
type: feat
status: completed
date: 2026-05-08
origin: docs/plans/2026-05-08-001-feat-authority-roadmap-plan.md
---

# feat: Add compute-only region placement

## Summary

Add the next authority-roadmap slice by making deploy placement region-aware while keeping all durable control-plane writes under the existing single authority. The implementation should teach placement policy that compute-region machines can run workloads, draining or disabled regions do not receive new work, and none of this creates regional authority ownership.

---

## Problem Frame

`docs/authority-roadmap.md` says regions answer "where can this run?" while authority answers "who owns truth?" The status and storage-promotion slices made the single-authority posture visible and useful; the next step is to let additional regions participate in compute placement without implying HA, DR, or multi-authority behavior.

---

## Assumptions

*This plan was authored from the user's accepted slice recommendation without a separate requirements document. The items below are agent inferences that should be reviewed before implementation proceeds.*

- This is a narrow U6a slice, not the full U6 roadmap unit.
- The first implementation should persist each machine's region role on the machine record instead of adding a separate region registry, region CLI, or region lifecycle API.
- Region role should affect workload placement only. Deploy commits, deploy status, instance records, NATS assets, and storage participation remain owned by `auth-default`.
- Existing deployed slots on draining machines should remain eligible to stay in place until an explicit movement or replacement decision occurs; draining affects new placements.

---

## Requirements

- R1. Active machines in the home data region and compute-only regions can receive new workload placement.
- R2. Disabled or draining regions do not receive new workload placement.
- R3. Existing slots on draining machines can remain in place when preserving them avoids rewriting historical deployment truth.
- R4. Deploy planning and execution continue to write durable control-plane records through the existing owning authority; region placement must not create a new authority or alter NATS replica policy.
- R5. If no eligible placement target exists, deploy planning fails visibly instead of silently scheduling onto an ineligible local fallback.
- R6. The implementation preserves current machine lifecycle behavior: machine-level `Draining` keeps existing slots but does not receive new slots; `Standby` is not a new placement target.

---

## Scope Boundaries

- Do not add regional authorities, cross-authority RPC, route export/import, or queued remote mutations.
- Do not add DR mirrors, read-local projections, mirror promotion, or async loss-window status.
- Do not add a full region management CLI/API in this slice.
- Do not make ordinary `machine add` change authority, quorum, storage participation, or NATS asset replica counts.
- Do not add background reconciliation that rewrites region placement or machine truth.

### Deferred to Follow-Up Work

- Region registry persistence and operator commands for changing an entire region between `home_data`, `compute`, `draining`, and `disabled`.
- Gateway/DNS-specific regional placement policy beyond the workload placement policy needed for deploy planning.
- Runtime reachability preflight for remote compute regions before deploy execution.
- Multi-authority or dev-authority promotion gates from U8.

---

## Context & Research

### Relevant Code and Patterns

- `VISION.md` requires explicit operator-triggered state changes, no hidden reconcilers, and foreground failures with a clear audience.
- `docs/authority-roadmap.md` defines authority as ownership, region as placement metadata, and compute regions as workload/gateway/DNS participants whose durable writes still go to the owning authority.
- `docs/plans/2026-05-08-001-feat-authority-roadmap-plan.md` U6 names the broader compute-only region target.
- `crates/ployz-types/src/model.rs` already has `RegionName`, `RegionRole`, `RegionRecord`, `MachineTopology`, `MachineMembership`, and `PlacementCandidate`.
- `crates/ployzd/src/daemon/handlers/machine/join/target.rs` and `crates/ployzd/src/daemon/handlers/mesh/bootstrap.rs` are the current seams where new machine records and bootstrap records can stamp authority/storage/topology intent.
- `crates/ployz-orchestrator/src/machine_policy.rs` is the current policy seam for new placement eligibility, existing-slot retention, coordination peers, and diagnostics.
- `crates/ployz-orchestrator/src/deploy/plan.rs` currently derives deployable machine IDs from stored machine membership before assigning replicated/global slots and volume placement.
- `crates/ployz-orchestrator/src/deploy/tests.rs` already has focused tests for deployable machines, current-slot reuse, draining behavior, global placement, and volume pinning.

### Institutional Learnings

- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`: keep stored truth separate from live observations and avoid fabricating replacement truth when data is missing.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`: commands that change authority, coordination, placement, or storage participation should validate the final participant set before mutation. This slice should apply that discipline to placement decisions even though it does not mutate authority.

### External References

- External research intentionally skipped. This work extends local Rust placement/deploy policy and does not introduce a new third-party API or NATS topology.

---

## Key Technical Decisions

- Keep this as placement policy, not authority policy: region eligibility filters workload targets but does not change store authority, NATS scope, storage participation, or replica policy.
- Store region role on machine membership for this slice, then extend the existing placement-policy seam rather than embedding region checks directly in deploy planning. This keeps placement decisions reusable by later gateway, DNS, migration, and machine-removal work without requiring a full region registry first.
- Treat "no eligible targets" as an operator-visible planning failure. A silent local fallback is useful for empty bootstrap-like inputs, but it is unsafe when the store has machines and policy says none are eligible.
- Preserve existing-slot retention separately from new placement eligibility. Draining remains a valid home for work that already exists, but it is not a target for fresh slots.

---

## Open Questions

### Resolved During Planning

- Should this slice build the full region registry and CLI? No. The first useful slice should persist a machine-level region role and use it for deploy-placement behavior; registry management can follow once the policy seam is proven.
- Should compute-only region placement create or imply a regional authority? No. `auth-default` remains the durable owner.
- Should external NATS or orchestrator research be done before planning? No. The repo already has the relevant authority roadmap, status learnings, and local placement policy seams.

### Deferred to Implementation

- Exact deploy error variant for no eligible placement targets: choose a structured error that fits the existing `DeployError` vocabulary and caller expectations.
- Whether the current local fallback should remain only for truly empty machine lists or be replaced entirely by explicit no-target errors after characterization tests clarify current bootstrap behavior.

---

## Implementation Units

### U1. Persist Machine Region Role

**Goal:** Add explicit per-machine region role to stored machine membership so placement policy can distinguish `home_data`, `compute`, `draining`, and `disabled` without inventing a full region registry.

**Requirements:** R1, R2, R4

**Dependencies:** None

**Files:**
- Modify: `crates/ployz-types/src/model.rs`
- Modify: `crates/ployz-api/src/machine.rs`
- Modify: `crates/ployzd/src/daemon/handlers/machine/join/target.rs`
- Modify: `crates/ployzd/src/daemon/handlers/mesh/bootstrap.rs`
- Test: `crates/ployz-types/src/model.rs`
- Test: `crates/ployzd/src/daemon/handlers/machine/tests.rs`

**Approach:**
- Add a region-role field to machine membership using the existing `RegionRole` enum. Keep the field separate from authority/storage participation so the model can represent "compute region under auth-default" directly.
- Stamp the founder/bootstrap machine as `home_data`.
- Stamp ordinary later joins as `compute` unless an existing explicit path already supplies a stronger region role. Ordinary add must still leave storage participation as candidate and must not raise NATS replicas.
- Surface the role through machine API/list payloads only where machine topology is already shown. Do not create region management commands in this slice.
- Keep serialization tests explicit. Do not use implicit default enum values to hide missing region-role intent.

**Execution note:** Start test-first with model serialization and machine-list/join expectations so the persisted shape is pinned before planner behavior depends on it.

**Patterns to follow:**
- Existing explicit `StorageParticipation` and `AuthorityNodePosture` modeling in `crates/ployz-types/src/model.rs`.
- Existing machine-list topology fields in `crates/ployz-api/src/machine.rs` and `crates/ployzd/src/daemon/handlers/machine/list.rs`.
- Prior machine-add invariant tests in `crates/ployzd/src/daemon/handlers/machine/tests.rs`.

**Test scenarios:**
- Happy path: the first bootstrap/founder machine is stored with region role `home_data`.
- Happy path: an ordinary added machine is stored with region role `compute` and `StorageParticipation::Candidate`.
- Edge case: a compute-region machine can still be storage-capable without becoming authority storage.
- Error-path prevention: adding a machine does not change authority, quorum, storage participation, or NATS asset replica count when the region role is compute.
- Serialization: machine membership JSON includes an explicit region role and rejects or handles missing role according to the project's current persisted-record policy.

**Verification:**
- Machine records carry explicit region role independently from authority posture.
- Existing status/machine-list surfaces still distinguish authority storage, storage candidate, and compute authority posture after the model change.

### U2. Add Region-Aware Placement Eligibility

**Goal:** Teach the shared placement-policy layer which machines are eligible for new placements and which machines can keep existing slots when region role and lifecycle are considered together.

**Requirements:** R1, R2, R3, R6

**Dependencies:** U1

**Files:**
- Modify: `crates/ployz-types/src/model.rs`
- Modify: `crates/ployz-orchestrator/src/machine_policy.rs`
- Test: `crates/ployz-types/src/model.rs`
- Test: `crates/ployz-orchestrator/src/machine_policy.rs`

**Approach:**
- Extend the policy input shape around `PlacementCandidate` so policy code can reason about machine lifecycle, topology, and the persisted machine region role without reaching back into full machine membership everywhere.
- Keep `is_new_placement_candidate` focused on new placement eligibility and `can_keep_existing_slot` focused on retention. Do not collapse both concepts into one "deployable" boolean.
- Treat `home_data` and `compute` as eligible region roles for new workload placement when the machine lifecycle is active.
- Treat `draining` and `disabled` region roles as ineligible for new workload placement. A draining machine or region can still keep an existing slot through the retention helper.
- Keep any compatibility/defaulting explicit and test-covered. Do not infer region authority ownership from region role.

**Execution note:** Add characterization tests for the existing lifecycle-only behavior before changing the policy shape, then extend the tests for region role.

**Patterns to follow:**
- Existing lifecycle split in `crates/ployz-orchestrator/src/machine_policy.rs`: active machines receive new placements; draining machines keep existing slots; standby machines are excluded.
- Existing explicit enum vocabulary in `crates/ployz-types/src/model.rs`; prefer `RegionRole` over new booleans.

**Test scenarios:**
- Happy path: an active machine in a `home_data` region is a new placement candidate.
- Happy path: an active machine in a `compute` region is a new placement candidate.
- Edge case: an active machine in a `draining` region is not a new placement candidate but can keep an existing slot.
- Edge case: an active machine in a `disabled` region is not a new placement candidate and cannot be chosen for new slots.
- Edge case: a machine with lifecycle `Draining` remains able to keep an existing slot even when it is not eligible for a new one.
- Error-path prevention: no policy helper infers authority ownership or storage participation from region role.

**Verification:**
- Placement-policy tests distinguish active home data, active compute, region-draining, region-disabled, machine-draining, and standby cases.
- The policy API remains reusable outside deploy planning.

### U3. Apply Region Eligibility to Deploy Planning

**Goal:** Make deploy planning select workload targets from region-eligible machines and report a visible planning failure when no eligible target exists.

**Requirements:** R1, R2, R3, R5, R6

**Dependencies:** U2

**Files:**
- Modify: `crates/ployz-orchestrator/src/deploy/plan.rs`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**
- Route deploy planning's machine selection through the shared region-aware placement helper from U1.
- Preserve deterministic ordering of selected machine IDs so deploy previews remain stable.
- For replicated placement, spread new slots only across eligible machines, while retaining existing slots on machines that can keep existing work.
- For global placement, create desired slots for eligible home data and compute region machines only.
- Revisit the current fallback that returns the local machine when no enabled candidates exist. Keep fallback behavior only where it genuinely represents an empty/bootstrap input, and otherwise return a structured planning error when stored machines exist but none are eligible.
- Keep volume pinning conservative: existing volumes bound to a machine that can keep existing work may remain; new volume placement should use the same eligible candidate set as new workload placement.

**Execution note:** Start with deploy-plan tests that expose current fallback and global-placement behavior, then change the planner.

**Patterns to follow:**
- Current `deployable_machines`, `desired_slots`, `new_volume_machine`, and `machine_is_deployable` separation in `crates/ployz-orchestrator/src/deploy/plan.rs`.
- Existing tests in `crates/ployz-orchestrator/src/deploy/tests.rs` for `deployable_machines_filters_by_participation`, draining slot retention, global placement, and volume-bound unavailable machine errors.

**Test scenarios:**
- Happy path: a replicated service with active home data and compute-region machines can place new slots on both eligible regions.
- Happy path: a global service creates one slot per eligible home data or compute-region machine.
- Edge case: a global service skips machines in region role `draining` or `disabled`.
- Edge case: an existing slot on a draining machine remains unchanged when the service revision and volume state do not require replacement.
- Error path: a replicated service fails planning when stored machines exist but every machine is in a disabled/draining region or lifecycle state.
- Error path: a new volume declaration fails or avoids placement when no eligible machine exists for the volume's attached services.
- Integration: deploy preview participants contain only eligible new-placement machines plus any retained existing-slot machines needed to remove or keep current work.

**Verification:**
- Deploy planning tests show compute-region machines are real placement targets without adding authority state.
- Existing lifecycle-based placement tests are updated intentionally, not broken accidentally by the region policy change.

### U4. Preserve Single-Authority Deploy Writes

**Goal:** Add regression coverage and light documentation so compute-region placement cannot be mistaken for regional durable ownership.

**Requirements:** R4

**Dependencies:** U1, U2, U3

**Files:**
- Modify: `docs/authority-roadmap.md`
- Modify: `docs/routing-and-deploys.md`
- Test: `crates/ployz-orchestrator/src/deploy/tests.rs`

**Approach:**
- Keep documentation concise: mark compute-region placement as implemented or initially supported, while naming the boundary that durable deploy writes still belong to `auth-default`.
- Add deploy-planning regression coverage that checks machine topology affects participant placement but does not introduce authority IDs, region-local stores, or NATS asset scope changes into the plan.
- Do not add status or CLI output for region health in this slice unless implementation already exposes it cheaply through existing machine-list fields.

**Patterns to follow:**
- `docs/authority-roadmap.md` current "Regions" and "Roadmap" sections.
- `docs/routing-and-deploys.md` existing bucket vocabulary for deploy commits, deploy status, instance records, and routing events.
- The authority-status learning's rule that operator-facing docs should separate stored truth from observations and projections.

**Test scenarios:**
- Happy path: deploy planning for a compute-region machine produces participant placement without changing storage participation or authority posture in the machine records used by the planner.
- Regression: route/deploy documentation continues to state that regions affect placement and do not create write authority.
- Test expectation: no daemon or NATS integration test is required for this unit unless implementation reaches beyond deploy planning.

**Verification:**
- Docs describe compute-only region placement without introducing DR, multi-authority, or queued remote intent.
- Tests keep authority ownership and placement eligibility as separate concerns.

---

## System-Wide Impact

- **Interaction graph:** Deploy preview/planning consumes machine membership through placement policy; runtime execution should see the same participant set without new daemon RPC surfaces.
- **Error propagation:** No eligible placement targets should become a structured deploy-planning failure, not a silent local fallback or log-only warning.
- **State lifecycle risks:** Existing slot retention must not rewrite stored deploy truth just because a region is draining; new placement and retention stay separate decisions.
- **API surface parity:** No new public CLI/API is expected in this slice. If implementation needs to expose new structured error variants, JSON/runtime serialization tests should cover them.
- **Integration coverage:** Deploy-plan tests must cover cross-layer behavior from stored machine records through preview participants and desired slots.
- **Unchanged invariants:** `auth-default` remains the durable owner; ordinary machine add and storage promotion semantics from the prior slices remain unchanged.

---

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Region role gets confused with authority ownership | Keep all region logic in placement policy and docs; add regression tests that placement does not change storage participation or authority state. |
| The current local fallback hides no-target failures | Characterize fallback behavior and preserve it only for empty/bootstrap inputs; stored-but-ineligible machines should fail visibly. |
| A full region registry sneaks into the slice | Defer region management commands and persistence to follow-up work unless implementation proves the policy cannot be represented without it. |
| Draining semantics regress | Keep separate helpers and tests for new placement eligibility vs existing-slot retention. |
| Volume placement chooses an ineligible compute target | Route new volume placement through the same eligible target set as new workload placement. |

---

## Documentation / Operational Notes

- Update `docs/authority-roadmap.md` to mark compute-region placement as initially supported only after tests prove deploy planning honors the boundary.
- Update `docs/routing-and-deploys.md` if needed to clarify that deploy commits/status remain stored intent under the owning authority even when participants are in compute regions.
- No rollout migration is expected; this is a planner/policy behavior change over existing machine topology records.

---

## Sources & References

- **Origin document:** `docs/plans/2026-05-08-001-feat-authority-roadmap-plan.md`
- Roadmap: `docs/authority-roadmap.md`
- Vision: `VISION.md`
- Related plan: `docs/plans/2026-05-08-002-feat-authority-status-slice-plan.md`
- Related plan: `docs/plans/2026-05-08-003-feat-nats-storage-promotion-plan.md`
- Institutional learning: `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`
- Institutional learning: `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`
- Related code: `crates/ployz-types/src/model.rs`
- Related code: `crates/ployz-orchestrator/src/machine_policy.rs`
- Related code: `crates/ployz-orchestrator/src/deploy/plan.rs`
- Related tests: `crates/ployz-orchestrator/src/deploy/tests.rs`
