# Liveness Surfaces At The Point Of Use

> The placement bullet is partially superseded by ADR 0035: a fresh placement
> bid is necessary, and fresh dataplane testimony must also pass.

Ployz never infers liveness from observation age and never lets an inferred
signal change cluster behavior. Every consumer of "is this machine alive"
has an exact, live answer at the moment it matters:

- Placement: the machine answers (or does not answer) the placement RPC —
  end-state, a bid it declines when draining. A dead machine cannot bid.
- Gateway serving: an offline machine's upstreams fail at dial time; the
  proxy handles the failed dial locally. The upstream set changes only when
  an operation changes it. This supersedes ADR 0009's staleness filter.
- DNS: answers change only when operations change records; a dead address
  fails at the client's connect. Freshness-filtered DNS was considered and
  rejected: at 1-2 replicas a false-stale inference (KV lag, NATS hiccup)
  pulls healthy capacity and causes the outage it was meant to soften.
  Revisit only with evidence that dead-address connect latency hurts in
  practice; prefer ordering answers over hiding them if it ever does.

Observation age remains display evidence — machine snapshots carry the raw
last-observed timestamp so operators and agents see silence as silence.
Behavior changes come from operations; an offline machine is just offline.
