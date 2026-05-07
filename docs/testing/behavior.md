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
- Certificate lifecycle tests pin renewal failure visibility: retryable
  finalize failures keep the certificate issuing and serving the previous
  active version until an explicit successful finalize clears the error.
- Instance status tests pin drain and runtime-failure transitions: drain clears
  stale errors, idempotent repeats preserve timestamps, and changed runtime
  failures update the visible error.
- Deploy orchestration tests pin that unreachable participants block before
  inspect, start, or commit, so reachability is checked at decision time rather
  than inferred from stored freshness.
- Machine placement policy and deploy-plan tests pin lifecycle semantics for
  orchestration: draining machines keep existing slots and remain blocking
  peers, but never receive new placements.
- Deploy preview tests pin the operator distinction between observation and
  mutation: unreachable participants are surfaced as warnings without writing
  deploy state.
- Managed-domain tests pin operator-facing TLS warning behavior: active
  certificates stay quiet, pending/issuing/missing certificates warn, and
  failed certificates include the stored `last_error`.
- Deploy export tests pin corrupt-state failure visibility: missing referenced
  revisions and mismatched stored service specs fail with explicit
  `deploy_export` errors.
- Deploy manifest decode tests pin foreground request failure visibility:
  invalid JSON returns an `INVALID_MANIFEST` daemon error with no misleading
  payload.
- Store API routing projection tests pin backend-independent event semantics
  across machines, revisions, releases, and instances: emitted routing events
  must update subscriber state the same way a fresh snapshot would, and replayed
  removals are idempotent by each collection's contract identity.
- Memory store routing event tests pin the reference backend contract:
  subscribers receive an initial snapshot followed by metadata-rich events
  whose payloads preserve old/new identity and satisfy acknowledgement
  semantics.
- Memory deploy commit tests pin backend write semantics: removed services emit
  release-removal events, and removed volumes are scoped to the deploy
  namespace.
- Memory machine registry tests pin removal visibility: deleting a machine
  updates the routing snapshot and emits both machine and routing removal
  events.
- Memory certificate store tests pin subscriber visibility for certificate and
  ACME challenge changes: initial snapshots, updates, and challenge removals
  are observable by background consumers.
- Store API routing acknowledgement tests pin foreground failure visibility:
  untracked events are no-ops, while closed ack receivers return an error to
  the caller instead of being hidden.
- NATS routing subscription tests pin transport failure visibility: routing
  events must carry event IDs and valid payloads, and malformed transport
  messages become subscriber errors instead of ambiguous projection input.
- NATS machine registry tests pin key/payload identity checks: malformed
  records and mismatched KV keys fail visibly instead of corrupting durable
  routing truth.
- NATS instance store tests pin key/payload identity checks: malformed instance
  records and mismatched KV keys fail visibly instead of emitting misleading
  routing updates or removals.
- Routing event projection tests pin key-only removals: removal events carry
  contract identity instead of stale record payloads, so deletes can be replayed
  idempotently from the explicit key.
- NATS deploy status tests pin key/payload identity checks: malformed or
  mismatched deploy status records fail visibly instead of being returned for
  the wrong deploy id.
- NATS invite store tests pin key/payload identity checks: malformed or
  mismatched invite records fail visibly before invite redemption or revocation
  mutates control-plane state.
- NATS certificate store tests pin key/payload identity checks for ACME
  accounts, certificate metadata, challenges, and readiness rows, so KV keys
  remain authoritative identities rather than untrusted decoded payload fields.
- NATS routing journal tests pin the NATS-native shape: each routing fact is a
  directly acknowledged JetStream message, with no staged batch/commit protocol
  or route-journal atomic publish dependency; event subjects are keyed directly
  by routing event id rather than a database-style batch/index pair.
- NATS asset manifest tests pin operator-visible control-plane inventory:
  every stream and KV bucket created by asset setup must appear in the status
  manifest with the correct stream/KV role.
- Runtime subscription tests pin that routing event acknowledgement failures
  are forwarded to the runtime reader as subscription errors instead of being
  swallowed by the daemon relay.
- Edge routing subscription tests pin failure visibility for DNS and gateway:
  applied routing events whose acknowledgements fail mark store-sync health
  unhealthy after publishing the latest snapshot.
- Daemon handler tests pin operator-facing lifecycle failures: invalid machine
  transitions return actionable errors and leave stored machine state unchanged.
- Machine remove tests pin mutating control-plane failure behavior: unreachable
  peers fail without `--force` and preserve the durable machine record.
- Runtime backend diff tests pin uncertainty handling: malformed observed
  container state must drift, and unknown liveness must recreate rather than
  silently adopting stale or ambiguous runtime state.
- Runtime parent-container tests pin network namespace safety: unknown or
  malformed observed parent labels do not satisfy expected parent identity.
- Runtime metrics snapshot tests pin observation uncertainty: unknown or
  malformed workload labels do not produce resource snapshots.
- ZFS inspection tests pin storage failure visibility: snapshot listing backend
  errors fail inspection instead of being reported as empty lineage.
- Status surface tests pin live-observation failures: missing or unreadable
  sidecar sync metrics report unknown health with an explicit error, while
  unhealthy metrics preserve stale-since and failure counts instead of
  pretending the edge is healthy.
- API serialization tests pin structured failure/status contracts: daemon
  responses preserve typed payloads, machine operation status, runtime
  subscription error frames, edge/control-plane uncertainty, and `last_error`
  across JSON roundtrips.
- SDK transport tests pin external-consumer contracts: stdio and Unix socket
  transports preserve the line protocol, malformed daemon output returns
  `std::io::Error`, and failed child processes do not become synthetic success.
- Runtime watch API tests pin backend-independent routing frames for all
  subscriber collections: machine, revision, release, and instance events map to
  stable upsert/remove keys.
- Machine operation tests pin durable failure visibility: a recorded operation
  failure remains visible through later running/stage updates and is cleared
  only by success.
- Volume transfer tests pin durable failure visibility: interrupted ZFS
  transfers preserve the prior failure error in operator-facing payloads.
- Machine add tests pin invite/precondition ordering: remote subnet mismatches
  fail before invite consumption.
- Mesh stop tests pin durable lifecycle truth under partial teardown failure:
  once the mesh runtime is destroyed, the stopped network lifecycle is
  persisted even if later sidecar shutdown reports an operator-visible error.
- Deploy apply tests pin commit boundaries: releases are not committed before
  required starts complete, and cleanup failures preserve a committed deploy
  while surfacing cleanup-pending state.
- Store API projection tests now cover machine, release, revision, and instance
  event families across add/update/remove semantics.
- Component health tests pin background failure visibility: unhealthy workers
  preserve their original stale-since timestamp, increment failure counts, and
  expose the latest error to status readers.
- Mesh background consumer tests pin subscription failure handling: peer sync,
  endpoint maintenance, eBPF route sync, and subnet-claim monitoring exit when
  machine subscriptions report errors instead of continuing against stale
  input.
- Deterministic simulator tests pin core orchestration behavior across seeded
  product event sequences: deploy lifecycle, gateway/DNS projection, volume
  ownership, node membership changes, failure/recovery, and intent/status/live
  observation separation.
