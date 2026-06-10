---
date: 2026-06-07
topic: namespace-succeed-or-die-operations
---

# Namespace Succeed-Or-Die Operations Requirements

## Summary

Ployz should simplify around namespace-scoped, succeed-or-die operations. Deploys submit a full namespace revision, acquire a short namespace lock, derive phases from Compose-like dependencies, promote each successful phase by updating the namespace serving target service set, run final cleanup for removed runtime material, and let later deploys reconcile reality from Docker.

Operations stop being durable workflows. Status is the public operation contract, `ops tail` is a best-effort internal transcript, and fresh-NATS recovery is approximate: infer enough from healthy Docker containers and labels to resume core operations, then let the next deploy repair drift.

---

## Problem Frame

The current NATS refactor plan leans toward JetStream durability as a correctness foundation: operation streams drive status projection, submission idempotency depends on retained stream sequence, serving target state is committed after deploy, and several streams/Object Store buckets are required before they carry real product behavior. That adds a large amount of code and test surface for cases the product can treat more directly.

The simpler product bet is that a mutating operation may die. If it dies, the next operation observes Docker, gateway state, route bindings, and labels, then plans from current reality. This aligns better with Ployz's goal of small-cluster primitives: bounded operations, clear status, useful failure evidence, and no hidden reconcilers.

This brainstorm is explicitly optimized for conceptual simplicity and LOC reduction.

---

## Key Decisions

- **Namespace is the deploy unit.** Deploy locks, diffs, phase promotion, and serving targets are all namespace-scoped.
- **Operations are not durable workflows.** There is no takeover/resume path, no event replay as correctness machinery, and no durable core idempotency guarantee.
- **Deploy locking prevents duplicate mutation.** A deploy must acquire a namespace lock with a 60 second TTL before creating an operation. If the namespace is busy, submit returns an API error and no operation is created.
- **Phase completion is promotion.** Each successful phase updates the namespace serving target entries for that phase's services. Later phase failure does not roll back earlier phase promotion.
- **Routes are service attachments.** Deploy specs may declare routes, and later route operations may attach or modify routes. Routes become active for a service when the service's phase promotes.
- **Removed services unpublish during cleanup.** Services removed from a namespace revision remain serveable until final cleanup performs serving unpublish, observes relevant role processes as warning evidence, and removes runtime containers.
- **Fresh-NATS rebuild is approximate.** Rebuild scans healthy Docker containers and labels to infer enough serving state to continue. Perfect reconstruction is not a product requirement.

---

## Requirements

**Operation Model**

- R1. Mutating operations must be treated as bounded attempts that may succeed, fail, or die without automatic resume.
- R2. Operation status must be written directly by the operation owner and must not be projected from operation events.
- R3. Operation events must be internal transcript entries for now, exposed through `ops tail` as best-effort retained output.
- R4. `ops tail` must replace the current `ops watch` framing and must not be documented as durable replay.
- R5. Operation events must not be exported as a stable public SDK/Cloud contract in the first cut.
- R6. Core submit idempotency must be removed as a correctness feature. Duplicate mutation is handled by explicit resource locks or by planning from observed reality.
- R7. Operation owner leases must be removed. Resource locks are the mechanism that bounds how long a dead attempt can block new work.
- R8. A dead or stale operation state may be inferred for display, but dead marking must not be required for correctness.

**Namespace Deploys**

- R9. A deploy request must carry the full desired namespace revision spec.
- R10. Deploy submit must acquire `deploy/<namespace>` before operation creation.
- R11. The namespace deploy lock must have a 60 second TTL and must be refreshed while the deploy worker is active.
- R12. If a namespace deploy lock is active, deploy submit must return `resource_busy` as an API error and must not create an operation record just to report the rejected submit.
- R13. Deploy planning must diff the full desired namespace revision against observed namespace runtime state.
- R14. The deploy planner must treat extra stopped or failed containers from older attempts as removable when they are not in the desired namespace spec.
- R15. A successful service update must retire replaced containers according to update order and stop grace period; extra services or containers outside the namespace revision are removed during final cleanup.
- R16. A failed phase must stop every container started by that phase and leave those stopped containers available for inspection.
- R17. A stop-first replacement that fails after stopping the old container must best-effort restart the old container.
- R18. The next deploy is the cleanup boundary for failed deploy evidence.

**Phases And Rollout**

- R19. Ployz must derive phases from service dependencies instead of requiring users to write manual phase lists.
- R20. Service dependencies must provide deploy ordering only; Compose lifecycle conditions such as `service_healthy` and `service_completed_successfully` are not core workflow semantics.
- R21. Missing dependencies and dependency cycles must fail planning.
- R22. Services whose dependencies are satisfied in the same layer belong to the same phase.
- R23. In each phase, Ployz must first run one canary mutation per service.
- R24. Canary mutations must run in parallel subject to per-machine concurrency.
- R25. Rollout of remaining mutations must begin only after every service canary in the phase succeeds.
- R26. The default rollout concurrency must allow up to 10 concurrent mutations per machine.
- R27. Canary placement must use the first planned replica mutation for each service, not a separate canary placement policy.
- R28. Update order must be automatic by default and overrideable per service.
- R29. Automatic update order must prefer `start-first` and choose `stop-first` when required by conflicts or unsafe shared local resources.

**Serving Target And Gateway**

- R30. A namespace serving target must be the current serveable service set for the namespace.
- R31. The deploy worker must update serving target entries for services in each successful phase.
- R32. Gateways must serve healthy containers that match the current namespace serving target, service route binding, and port.
- R33. Role observation windows must be bounded warning-only checks after routed promotion or serving unpublish and must not decide deploy success.
- R34. A deploy may continue after a role observation window even when relevant role processes do not report convergence.
- R35. Route bindings must attach to a service within a namespace.
- R36. Deploy-declared routes for a service must become active when that service's phase promotes.
- R37. Route operations must be able to attach, detach, or modify routes after deploy.
- R38. Route labels on containers may be used for approximate rebuild, but normal gateway behavior must use route records and the serving target.
- R39. Serving unpublish for removed routed or DNS-published services must happen before Docker cleanup; missing role-process convergence is warning evidence, not cleanup failure.

**Rebuild And Disposable JetStream**

- R40. JetStream KV, streams, and Object Store must not be treated as the only copy of cluster truth unless a record is explicitly named as durable authority.
- R41. Fresh-NATS rebuild must scan Docker, group healthy Ployz containers by namespace, and infer an approximate current revision.
- R42. Rebuild may infer approximate route bindings from container labels.
- R43. Rebuilt targets and routes must be marked as inferred or degraded.
- R44. Rebuild does not need to reconstruct exact phase cursors, prior operation history, or exact route intent.
- R45. The next deploy or route operation must be the repair mechanism after approximate rebuild.
- R46. NATS loss must lose operation memory and transcript history, not the ability to submit new core operations.

**Simplicity And LOC Reduction**

- R47. The implementation plan must target deletion or major shrinkage of operation status projection, durable submission indexes, owner leases, overbuilt serving-target commit/CAS, and unused required JetStream resources.
- R48. The plan must prefer direct status writes, namespace locks, and observed-runtime planning over replay, takeover, or recovery machinery.
- R49. The first implementation plan must not add replacement complexity that cancels the LOC reduction, such as a generic workflow engine, durable consumers for operation takeover, or a canonical revision store.

---

## Key Flows

- F1. Deploy accepted
  - **Trigger:** A client submits a deploy for a namespace.
  - **Steps:** Validate the full namespace spec; acquire the 60 second namespace deploy lock; create operation status; start the deploy worker.
  - **Outcome:** The client receives an operation id only after the namespace lock is acquired.

- F2. Namespace busy
  - **Trigger:** A client submits a deploy while the namespace deploy lock is active.
  - **Steps:** Lock acquisition returns busy.
  - **Outcome:** The API returns `resource_busy`; no operation is created.

- F3. Phase promotion
  - **Trigger:** All canaries and remaining rollout mutations for a phase are healthy or complete.
  - **Steps:** Update the namespace serving target entries for the phase's services; update operation progress; emit transcript events; run relevant role observation windows as warning-only evidence; continue to the next phase if any.
  - **Outcome:** Gateways may serve the newly promoted service entries, and later deploy failure will not roll that phase back.

- F4. Phase failure
  - **Trigger:** A canary or rollout mutation fails in a phase.
  - **Steps:** Stop every container started by that phase; best-effort restart old containers for failed `stop-first` replacements; write deploy outcome and per-service deploy results.
  - **Outcome:** The namespace serving target keeps previously promoted service entries, stopped failed containers remain for inspection, and the deploy outcome is `failed` or `partially_completed` based on whether any phase promoted.

- F4a. Final cleanup
  - **Trigger:** Desired service phases have finished and runtime material outside the namespace revision remains.
  - **Steps:** For routed or DNS-published removals, perform serving unpublish and observe relevant role processes as warning evidence; stop and remove runtime containers with the service stop grace period.
  - **Outcome:** Removed services get `removed` results only after runtime containers are removed. Cleanup failure makes the namespace outcome `failed` or `partially_completed` using the same phase-promotion rule.

- F5. Fresh-NATS rebuild
  - **Trigger:** NATS and JetStream state are lost and a fresh server starts.
  - **Steps:** Nodes scan Docker; healthy managed containers are grouped by namespace; current revision and routes are inferred from labels where possible.
  - **Outcome:** Ployz publishes inferred serving state so core operations can continue, with exact repair left to the next deploy or route operation.

---

## Acceptance Examples

- AE1. **Deploy worker dies mid-deploy**
  - **Covers:** R1, R11, R14, R46
  - **Given:** A deploy has acquired `deploy/prod` and starts containers, then the worker dies.
  - **When:** More than 60 seconds pass and a user submits a new deploy for `prod`.
  - **Then:** The new deploy can acquire the namespace lock, observe Docker, and plan from current runtime state.

- AE2. **Duplicate deploy submit**
  - **Covers:** R6, R10, R12
  - **Given:** A client retries a deploy submit after losing the response.
  - **When:** The first deploy still holds the namespace lock.
  - **Then:** The retry returns `resource_busy` and does not create a second operation.

- AE3. **Failed canary**
  - **Covers:** R23, R25, R16, R17
  - **Given:** A phase contains `api`, `web`, and `worker`.
  - **When:** The `web` canary fails health.
  - **Then:** Ployz stops every container started by that phase, restarts any old `stop-first` container best-effort, does not promote the phase, and records a `failed` or `partially_completed` deploy outcome based on prior promoted phases.

- AE4. **Later phase failure after earlier promotion**
  - **Covers:** R31, R34, R45
  - **Given:** Phase 1 and phase 2 of revision 12 have promoted.
  - **When:** Phase 3 fails.
  - **Then:** The deploy outcome is `partially_completed`, and the namespace serving target keeps the service entries promoted by phases 1 and 2.

- AE5. **Successful cleanup**
  - **Covers:** R15, R18
  - **Given:** A previous failed deploy left stopped containers for revision 11.
  - **When:** A deploy of revision 12 succeeds.
  - **Then:** Runtime containers not in the namespace revision are removed during final cleanup, after any serving unpublish needed for removed routed or DNS-published services.

- AE6. **Approximate rebuild**
  - **Covers:** R41, R42, R43, R44
  - **Given:** JetStream state is deleted but Docker still has healthy Ployz containers.
  - **When:** Fresh-NATS rebuild runs.
  - **Then:** Ployz infers a current namespace revision from healthy labels, marks the result inferred, and does not promise exact prior operation or route state.

---

## Success Criteria

- The new requirements let a planner remove or shrink the largest operation-complexity areas instead of migrating them into a new abstraction.
- Deploy correctness depends on namespace locks, Docker observations, phase health, serving target updates, and cleanup, not operation event replay.
- A fresh NATS server can become operable without restoring all JetStream history.
- A reader can explain the deploy model as: lock namespace, diff namespace, execute derived phases, promote each successful phase into the serving target, run final cleanup, and let later deploys repair drift.
- The plan that follows should show clear LOC reduction opportunities in operation projection, submission idempotency, owner lease handling, overbuilt serving-target commit/CAS, unused streams, and Object Store defaults.

---

## Scope Boundaries

- Durable workflow takeover is out of scope.
- Public stable operation event schemas are deferred.
- Exact disaster restoration from Docker labels is out of scope.
- A canonical JetStream revision store is out of scope.
- Per-service deploy locks are out of scope for the first cut.
- Manual phase lists are out of scope for the first cut.
- Queueing deploys behind locks is out of scope.

---

## Dependencies And Assumptions

- Docker labels must carry namespace, revision, phase, service, and enough route hints for approximate rebuild.
- Route records must exist separately from the namespace serving target during normal operation.
- Cloud may keep richer revision history and workflow idempotency outside core.
- The 60 second deploy lock TTL is an intentional product choice, not a placeholder.
- The current repository is still early enough that breaking operation API compatibility is acceptable.

---

## Sources

- `VISION.md`
- `AGENTS.md`
- `docs/plans/2026-06-04-001-refactor-nats-greenfield-control-plane-plan.md`
- `.workflow/jetstream-disposable-audit/final_report.md`
- External comparison: Uncloud deploy behavior for live-container diffing, ordered container operations, and failed-container retention.
