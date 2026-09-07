# Bound Global placement to commands

Global Services are placed by Deploy and membership-command catch-up; the daemon
no longer maintains their slots in the background. This removes the machine-local
convergence exception in DESIGN.md: Docker retains responsibility for configured
container restarts, while deleted slots, later eligibility changes, and transient
placement failures require explicit redeployment.

Join catch-up observes target storage, reports unknown eligibility and incomplete
placement without undoing membership, and keeps fresh target-local admission.
Ingress remains a Global Service and participates in the same bounded catch-up.
