# Deploys Are Namespace Reconciliation Attempts From Runtime State

> Superseded for current v2 by [ADR 0041](0041-preferred-controller-serializes-cluster-mutations.md), which uses one bounded sole-service deploy instead of the revision/phase planner below.

Ployz deploys attempt to make one namespace match a normalized namespace revision derived from deploy input. A deploy observes runtime state, computes a phase-ordered deploy plan, executes bounded changes, promotes each successful phase by updating serving target entries for that phase's services, and leaves later repair to the next deploy or explicit operation.

Deploy correctness depends on namespace locks, runtime observations, service container health, and promotion, not on replaying old operation history.
