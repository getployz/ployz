# Behavior-First Test Strategy

Ployz tests should encode product promises, not implementation shape. The
highest-value tests are the ones that make it difficult to accidentally change
what an operator, daemon, backend, or SDK consumer can rely on.

## Promises To Protect

- Orchestration operations complete or fail clearly before durable truth is
  advanced.
- Lifecycle state changes happen through explicit transition records with
  evidence, timestamps, and invalid-transition errors.
- Failure has a reader: foreground failures return `Result`, background work
  preserves prior state and surfaces health or observations for the next
  reader.
- Core orchestration logic stays backend-independent. Runtime, store, and
  transport backends implement contracts; they do not define policy.
- Operator-facing surfaces keep intent, stored status, and live observation
  separate.

## Where Tests Belong

- Pure model tests for lifecycle transitions, persisted state contracts, and
  projection helpers.
- Deterministic simulator tests for core orchestration behavior: seeded product
  event sequences, fake clocks/backends, projection invariants, and final
  convergence checks.
- Orchestrator tests with memory stores, fake participant clients, and fake
  probes for deploy planning, fail-fast coordination, and commit boundaries.
- Daemon handler tests for command behavior, preconditions, and operator-facing
  errors.
- Backend contract tests for runtime/store behavior that must remain identical
  across implementations.
- E2E tests only for behavior that needs real process, network, container, or
  storage boundaries.

## Deterministic Simulation Boundary

The simulator goal is to protect Ployz core orchestration behavior, not to
replace runtime backend tests. It should model product events against fake
time, fake store/NATS, fake runtime observations, and fake WireGuard state,
then check externally meaningful invariants over the model and projections.

Good simulator coverage answers questions like:

- Does a deploy expose only ready, non-draining instances through gateway and
  DNS projection?
- Do node membership changes leave volume ownership, release slots, endpoint
  selections, and runtime status coherent?
- Do failure and recovery sequences keep durable intent, stored status, and
  live observation separate?
- Can a seed reproduce the same operation sequence and invariant failure?

The simulator should not claim that Docker, ZFS, WireGuard, iptables, NATS,
DNS sockets, or gateway networking work correctly. Those are runtime/backend
and E2E concerns. The simulator may use fakes for those systems only to drive
and validate core orchestration decisions.

Take inspiration from TigerBeetle's deterministic testing shape: seed-derived
workloads, explicit fake time and I/O boundaries, continuous checkers, compact
failure reports with the seed and operation history, and final convergence
passes. Avoid assertions that depend on incidental internal event ordering.

Run the simulator coverage with:

```sh
cargo test -p ployz-sim
```

## Covered Behavior Slices

- Machine lifecycle tests pin explicit activation, drain, standby, idempotency,
  and invalid-transition behavior in `ployz-types`.
- Deploy lifecycle tests pin explicit commit and cleanup-pending transitions,
  including idempotent retries that preserve the original completion evidence.
- Instance status tests pin drain and runtime-failure transitions: drain clears
  stale errors, idempotent repeats preserve timestamps, and changed runtime
  failures update the visible error.
- Deploy orchestration tests pin that unreachable participants block before
  inspect, start, or commit, so reachability is checked at decision time rather
  than inferred from stored freshness.
- Deploy preview tests pin the operator distinction between observation and
  mutation: unreachable participants are surfaced as warnings without writing
  deploy state.
- Store API routing projection tests pin backend-independent event semantics
  across machines, revisions, releases, and instances: emitted routing events
  must update subscriber state the same way a fresh snapshot would.
- Memory store routing batch tests pin the reference backend contract:
  subscribers receive an initial snapshot followed by metadata-rich batches
  whose events preserve old/new identity and satisfy acknowledgement semantics.
- Store API routing acknowledgement tests pin foreground failure visibility:
  untracked batches are no-ops, while closed ack receivers return an error to
  the caller instead of being hidden.
- Runtime subscription tests pin that routing batch acknowledgement failures
  are forwarded to the runtime reader as subscription errors instead of being
  swallowed by the daemon relay.
- Daemon handler tests pin operator-facing lifecycle failures: invalid machine
  transitions return actionable errors and leave stored machine state unchanged.
- Machine remove tests pin mutating control-plane failure behavior: unreachable
  peers fail without `--force` and preserve the durable machine record.
- Runtime backend diff tests pin uncertainty handling: malformed observed
  container state must drift, and unknown liveness must recreate rather than
  silently adopting stale or ambiguous runtime state.
- Status surface tests pin live-observation failures: missing or unreadable
  sidecar sync metrics report unknown health with an explicit error instead of
  pretending the edge is healthy.
- API serialization tests pin structured failure/status contracts: daemon
  responses preserve typed payloads, machine operation status, runtime
  subscription error frames, and `last_error` across JSON roundtrips.
- Machine operation tests pin durable failure visibility: a recorded operation
  failure remains visible through later running/stage updates and is cleared
  only by success.
- Machine add tests pin invite/precondition ordering: remote subnet mismatches
  fail before invite consumption.
- Deploy apply tests pin commit boundaries: releases are not committed before
  required starts complete, and cleanup failures preserve a committed deploy
  while surfacing cleanup-pending state.
- Store API projection tests now cover machine, release, revision, and instance
  event families across add/update/remove semantics.
- Component health tests pin background failure visibility: unhealthy workers
  preserve their original stale-since timestamp, increment failure counts, and
  expose the latest error to status readers.
- Deterministic simulator tests pin core orchestration behavior across seeded
  product event sequences: deploy lifecycle, gateway/DNS projection, volume
  ownership, node membership changes, failure/recovery, and intent/status/live
  observation separation.
