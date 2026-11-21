---
name: verify-service-floor
description: Model-check a Service's floor on one Machine (a forget raising it, the first floor read and the recheck of a new Container's admission, an Operator's start of a kept Container, a key removal, a crash) with the Stateright model in core/crates/ployz-verify/src/service_floor.rs. Use when a change touches core/crates/ployz-decide/src/service_floor.rs, ConsumptionLog's floor, raise_floors or floor_key in core/crates/ployzd/src/machine/local_machine/consumption.rs, forget.rs, AdmittedBy, Standing::admits, approved_versions or full_access in admission.rs, rule_floor in rule_deploy.rs, the floor check in rule_grant.rs, or verify_forget in core/crates/ployz-core/src/authority/version.rs.
---

# Verify the service floor

The model in `core/crates/ployz-verify/src/service_floor.rs` explores every interleaving of one Machine's floor for one Service: forgets signed honestly and eleven minutes ahead, admissions of new Containers under versions and rules (first floor read, pull, recheck before create), Operator starts of kept Containers, the removal of a key, the clock and a crash. Each floor read calls the `ployz_decide::service_floor` function the daemon calls. The module doc names the code each step copies.

The properties, in one sentence each:

- No create or Operator start passes its last floor read under an approval that floor forgot.
- No admission pulls after a first floor read that forgot its approval.
- The floor is always the signed time of a forget this Machine took.
- The floor never falls.

A check holds to the floor it read. A forget acknowledged between a passing read and Docker's start is the accepted handoff race, not a break.

## Run it

```
cd core && cargo test -p ployz-verify service_floor -- --nocapture
```

A few seconds. Green means the shipped test passes every property and reaches every `sometimes`, and each mutation test breaks its property:

- `the_recheck_reads_the_floor`: `still_holds` and `Standing::admits` skip the floor.
- `a_verified_build_reads_the_floor_too`: `rule_floor` runs only for a build checked online. The recheck still refuses the create, so this guard spares only the pull.
- `each_key_keeps_its_own_time`: a kept admission keeps one newest time for all its keys.
- `a_forget_ahead_is_refused_not_clamped`: a forget past the guard raises the floor to the guard.

## Real bug or model error

A counterexample is a lead. Check each step of the printed story against the function the module doc names. A step the code can't take is a model error: fix the model and say so. If every step matches, it is a finding. Keep the property.

## Bounds and blind spots

One Machine, one Service with a sibling at no floor, two honest forgets, one forget ahead, five approvals, two kept Containers, one crash, a clock that reaches two minutes. Out of scope: a second Machine, the forget fan-out, the join that hands floors to a new Server, and signature checks.
