# Operations Are Informational Records, Not Workflows

Ployz operations are user-visible records of bounded command attempts, not durable workflows that must be replayed, resumed, or taken over for correctness. Operation status, transcripts, and evidence explain what was attempted and what happened; future deploys and repairs plan from runtime state, not from prior operation logs.

There is no durable event replay for correctness, no owner leases, no durable core submit idempotency, and no automatic workflow takeover. A dead operation is evidence; the next operation observes reality and proceeds.
