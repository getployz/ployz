# Deploys Are Namespace Reconciliation Attempts From Runtime State

Ployz deploys attempt to make one namespace match a normalized namespace revision derived from deploy input. A deploy observes runtime state, computes a phase-ordered deploy plan, executes bounded changes, promotes each successful phase by updating serving target entries for that phase's services, and leaves later repair to the next deploy or explicit operation.

This adopts Uncloud's useful "plan from what is running, then fix and rerun" mechanic while keeping Ployz's stronger namespace, phase promotion, operation evidence, and cert-gated route model. Deploy correctness depends on namespace locks, runtime observations, service container health, and promotion, not on replaying old operation history.
