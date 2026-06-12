# Operations Are Informational Records, Not Workflows

Ployz operations are user-visible records of bounded command attempts, not durable workflows that must be replayed, resumed, or taken over for correctness. Operation status, transcripts, and evidence explain what was attempted and what happened; future deploys and repairs plan from runtime state, not from prior operation logs.

This decision supports removing durable event replay as a correctness mechanism, owner leases, durable core submit idempotency, and automatic workflow takeover. A dead operation is evidence; the next operation observes reality and proceeds.
