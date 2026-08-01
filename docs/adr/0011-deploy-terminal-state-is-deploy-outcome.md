# Deploy Terminal State Is Deploy Outcome

Ployz deploy operations should use a deploy outcome as their terminal state instead of forcing every terminal deploy into generic completed or failed buckets. A namespace deploy can fully complete, complete with warnings, partially complete through one or more promoted phases, fail before useful namespace progress, or be cancelled.

Per-service deploy results should be first-class evidence so Cloud and CLI can show which services completed, failed, skipped, or stayed unchanged within the namespace-level outcome; warning evidence belongs to the namespace deploy outcome and operation events.
