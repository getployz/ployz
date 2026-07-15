# Internal DNS Bind Readiness

## Finding

The transient failure in
[Dataplane/internal-DNS ~30s warmup race after deploy](https://github.com/getployz/ployz/issues/411)
has two code-level causes:

- The DNS resolve RPC sampled resolver health once. A request arriving while the
  resolver reported `AwaitingBind` failed immediately with `internal DNS
  resolver is not bound`, even though the resolver's bind retry task was
  already running.
- Bind retries started at 250 milliseconds and doubled to a 30-second ceiling.
  At the observed `awaiting-bind(attempts=10)`, the retry loop had already
  reached that ceiling. Once the environmental bind condition cleared, the next
  attempt could therefore remain asleep for roughly 30 seconds.

The preserved run did not record the bind errno or interface state. It cannot
distinguish an address that was not assigned yet from an address already in use
or another host-level bind failure. The correction is intentionally independent
of that missing classification.

## Decision

An internal DNS resolve request waits up to three seconds for an
`AwaitingBind` resolver to become `Serving`. It polls the existing local
health cell every 25 milliseconds without holding the mutex across an await. A
resolver that remains unbound returns the existing typed unavailable response.
An unconfigured resolver still fails immediately.

The resolver continues to use exponential bind backoff, capped at one second.
This avoids a tight retry loop while bounding the delay between the host
becoming bindable and the next attempt.

The control-side resolve gather has one shared 30-second deadline, not an
independent deadline per machine. It gathers at most 16 machine responses
concurrently. During a persistent all-machine bind outage, `network resolve`
now takes about three seconds per batch of 16 before reporting failures. At
more than roughly 160 machines, the shared deadline can expire before every
batch receives the clean not-bound response. This is an accepted diagnostic
tradeoff for the product's 1–200-machine range: the common startup race is
absorbed, while every request remains bounded.

## Rejected Boundaries

Deploy completion does not wait for DNS readiness. DNS is independently
supervised data plane, and the same bind transition occurs after a DNS role
restart. Coupling deploy success to this local substrate condition would mask
only one caller context and would make deploy own readiness outside its
operation boundary.

A new notification abstraction is also unnecessary. Resolver health has one
writer, two readers, and one forward transition from awaiting bind to serving.
The bounded local poll keeps that shape explicit.

## Deterministic Evidence

The regression test reserves a UDP address, starts the real resolver so its
first bind fails, releases the address, and invokes the real resolve handler
while health is still `AwaitingBind`. Before the correction it failed
immediately with the exact real-host message. After the correction it waits for
the retry and receives a DNS response.

```sh
cargo test -p ployzd \
  roles::dns::service::tests::resolve_waits_for_an_initially_unbound_resolver \
  -- --exact --nocapture

cargo test -p ployzd \
  roles::dns::internal::tests::resolver_bind_retry_stays_at_a_one_second_cadence \
  -- --exact --nocapture
```
