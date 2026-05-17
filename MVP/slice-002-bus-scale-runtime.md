# Slice 002: Bus Scale Runtime

Status: implemented

## What Shipped

- Shared `Payload` bytes so fanout clones are handle clones, not repeated byte
  allocations.
- A bus-wide bounded delivery runtime with configurable worker count, queue
  capacity, drain accounting, and max-concurrency metrics.
- A Kameo `BusActorHandle` facade for actor-owned bus access.
- Actor operations delegate blocking bus work out of the Kameo mailbox, so a
  slow request does not block unrelated actor messages.
- Structured E2E metrics with latency percentiles and process memory snapshots.
- A `scale` E2E scenario that runs 200, 1,000, and 10,000 logical-node cases.
- An `actor-contract` E2E scenario that exercises the Kameo facade directly,
  including no responders, timeouts, authorization failures, and drain.
- A maintainer-facing primitive map in `MVP/primitive-decisions.md`.

## Proof

Generated proof:

- Command: `cd MVP && cargo run -p mvp-e2e -- scale`
- Artifact: `MVP/target/mvp-e2e/scale-metrics.json`
- Snapshot: local development run captured while implementing this slice.

| Logical nodes | Publish deliveries | Request-many replies | Publish p99 | Request-many p99 | Runtime max concurrency |
| --- | ---: | ---: | ---: | ---: | ---: |
| 200 | 20,000 | 20,000 | 2,063us | 2,117us | 64 |
| 1,000 | 100,000 | 100,000 | 5,927us | 10,335us | 64 |
| 10,000 | 1,000,000 | 1,000,000 | 49,791us | 92,095us | 64 |

The same run includes `actor-contract`, plus a saturation case with 4 concurrent
publishers, 96 slow subscribers, 4 delivery workers, and delivery queue capacity
8. It observed 384 deliveries, max worker concurrency 4, max queued deliveries
8, 309 full-queue enqueue waits, 4.99s total enqueue blocking, and
`bounded_backpressure_observed = true`.

The 10,000-node run is a logical-node stress test inside one process. It proves
the bus semantics, payload sharing, bounded delivery execution, response
aggregation, and metrics path. It does not claim real iroh transport behavior.

## Semantics Check

Business logic now gets the higher-level operations directly:

- `publish(gateway.changed)`
- `request(node.alpha.inspect)`
- `request_many(node.*.capacity)`
- `queue_subscribe(deploy.submit, schedulers)`
- `drain()`

The code that expresses those operations does not need to manage delivery
threads, response inboxes, queue group selection, wildcard matching, response
authorization, or drain accounting. That is the semantic leverage this rewrite
is meant to test.

## Simplicity Notes

- The Kameo actor is a facade over the bus, not a second implementation.
- Blocking request/publish/drain work runs outside the actor mailbox; the actor
  owns admission and dispatch semantics, not long waits.
- Runtime metrics are exposed through one snapshot type instead of spread across
  the E2E harness.
- The delivery runtime owns execution; the bus lock is only used to validate
  grants and select deliveries.
- The primitive decision doc is intentionally short. If it grows too large,
  split it into per-primitive ADRs later.

## Next Follow-Up

- Make actor mailbox/backpressure policy explicit once more subsystems move
  behind actors.
- Add service registry and queue/service discoverability tests before distributed
  transport.
- Keep updating `MVP/primitive-decisions.md` when a slice picks a new crate or
  primitive.
