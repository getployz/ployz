# Serving Eligibility Uses Fresh Observations, Not Durable Membership

> Partially superseded by ADR 0027: observation freshness remains evidence
> for warnings and diagnostics, but it no longer filters gateway upstreams
> or any other serving behavior — liveness surfaces at the point of use.

Ployz should derive warning-only role visibility from fresh role observations instead of durable membership tables. Known gateways, DNS processes, and machine agents are role processes with recent observations; stale role processes age out of role observation windows and diagnostics without a cleanup operation.

This supports the disposable JetStream model and keeps fresh-NATS recovery simple: role processes reconnect, observe current state, and publish fresh observations. The trade-off is that deploy completion, routed promotion, serving unpublish, and diagnostics must treat missing or stale role observations as evidence and warnings, not as durable quorum state.
