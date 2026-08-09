# Deploy Terminal State Is Deploy Outcome

> Superseded for current v2 by [ADR 0041](0041-preferred-controller-serializes-cluster-mutations.md), whose deploy outcomes are only completed, failed, or interrupted; the richer phase results below are historical.

Ployz deploy operations should use a deploy outcome as their terminal state instead of forcing every terminal deploy into generic completed or failed buckets. A namespace deploy can fully complete, complete with warnings, partially complete through one or more promoted phases, fail before useful namespace progress, or be cancelled.

Per-service deploy results should be first-class evidence so Cloud and CLI can show which services completed, failed, skipped, or stayed unchanged within the namespace-level outcome; warning evidence belongs to the namespace deploy outcome and operation events.
