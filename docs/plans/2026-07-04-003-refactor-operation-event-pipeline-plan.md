# Operation events own their identity; one namespace commit port

## Context

A cold architecture review found two shallow-layer stacks around deploy
and operation recording:

1. Adding one operation event touches five files across two crates
   (enum variant, free subject fn, adapter subject match, append
   constructor, repository wrapper), and the event's durable identity —
   its subject and its idempotent message id, including the invariant
   that all terminal deploy transitions share one dedup id — lives in
   the NATS adapter instead of next to the event definition.
2. `RouteBindingCommitter` and `ServingTargetCommitter` are one seam
   split in two: identical guard-then-delegate impls on the single
   production adapter, mirrored error enums, six type parameters on
   `execute_deploy_operation`.

Grilled decisions:

- Dead write surface is deleted, not ported: the four cert `record_*`
  methods and the `OperationEventAppend` constructors have zero
  production callers (the cert controller is already parked in git
  history); `record_machine_add_joined` / `record_machine_add_completed`
  turned out to have join-flow callers and stay. The
  cert **read model** (event variants, classification, projection,
  ployz rendering) stays — it is the schema of a staged feature and
  exhaustively matched. Recording returns with the PR that wires cert
  renewal, as `record_cert_transition(CertTransition)`.
- The committer merge folds the namespace-lock guard into the port's
  contract: lock-checked commit becomes the only constructible
  production path, deleting the test-only unguarded mode.
- Subjects and message ids are durable-stream contracts: the move must
  be byte-identical, pinned by golden tests in core. One deliberate
  exception: `OperationEvent::Cancelled` (never produced by any writer)
  gains a `kind` field so a cancel shares its operation kind's terminal
  dedup id; kind-blind cancellation would let a cancel race another
  terminal write past the stream-level finality guarantee.

## Changes

### 1. Event identity moves onto `OperationEvent` (ployz-core)

- `OperationEvent::subject(&self) -> String` replaces the 25 per-event
  free functions in `subjects.rs` and the 26-arm
  `operation_event_subject` match in the adapter. `op_watch` (the one
  externally used pattern fn) and any stream-pattern fns stay.
- `OperationEvent::message_id(&self) -> String` absorbs
  `submitted_message_id` / `transition_message_id` /
  `evidence_message_id`, including the terminal-transitions-share-one-id
  invariant, with a doc comment stating that invariant (dedup enforces
  "terminal states are final" at the stream level).
- Golden pin test in `crates/ployz-core/tests/` — one representative
  event per family asserting the exact subject and message-id strings
  rendered today.
- `OperationEventAppend` keeps only `from_event(event)`; the adapter
  derives subject and message id from the event. The 25 constructors
  die.

### 2. Writers record transitions (ployz-nats repository)

- `record_machine_update_running/completed/failed` collapse into
  `record_machine_update_transition(operation_id, machine_id,
  MachineUpdateTransition)` mirroring `record_deploy_transition`; the
  three call sites in `machine_update_runtime.rs` pass the variant.
  `Cancelled` gets a writer for free when cancellation lands.
- Deleted (caller-less): `record_cert_challenge_published`,
  `record_cert_validation_started`, `record_cert_completed`,
  `record_cert_failed`, and their append constructors and ployz-nats
  tests.
- Stays: `record_deploy_transition` / `record_deploy_evidence` (already
  the deep shape), the mint worker's
  `record_machine_add_credential_provisioned` /
  `record_machine_add_failed` (production-called, domain-rich), the
  join-flow internals in `repository/machine_join.rs`, and submission.

### 3. One namespace commit port (ployzd deploy worker)

- `RouteBindingCommitter` + `ServingTargetCommitter` merge into one
  `NamespaceStateCommitter` trait: `replace_route_binding`,
  `remove_route_binding`, `replace_serving_target_entry`,
  `remove_serving_target_entry`, one error enum `NamespaceCommitError`
  with variant-specific subjects (route targets vs commit scopes) —
  `ControlPlaneCommitScope` is wire-exported and has no route variant,
  so the merge stays internal rather than widening the wire type.
- The namespace-lock guard is the port's contract:
  `LockCheckedCoreState` becomes the only production adapter and states
  the guard once; the `namespace_lock_lost: Option<…>` mode on
  `run_deploy_operation` and its duplicated call arms are deleted
  (the `None` arm was test-only).
- `execute_deploy_operation` drops a type parameter; the paired
  route/active-state fakes in `deploy_operation/fixtures.rs` collapse
  into one committer fake covering all commit-failure scenarios.

## Not in scope

- Cert write path (returns with the cert-renewal PR).
- The mint worker's `records()` piercing and the Machine Usability View
  (separate candidates).
- Any wire or stream-format change beyond the never-produced
  `Cancelled` variant gaining `kind`: subjects, message ids, and event
  JSON are byte-identical everywhere else.

## Verification

1. Golden pins: subject + message-id strings per event family match the
   pre-refactor renderings (write the pins against current main first,
   then refactor under them).
2. Full suite green (`--exclude ployz-e2e --exclude ployz host`),
   zero workspace warnings — proves the deleted wrappers had no live
   callers.
3. Grep gates: no `op_cert_`/per-event `op_deploy_`/`op_machine_` free
   fns remain (op_watch stays); `RouteBindingCommitter`/
   `ServingTargetCommitter` appear nowhere; `namespace_lock_lost:
   Option` gone.
4. NATS integration suites for operations, deploy runtime, and machine
   update pass — the dedup/idempotency behavior is exercised against a
   real JetStream stream.
5. `packages/ployz-sdk/src/generated.ts` changes only for the
   `Cancelled` variant (`kind` field and the `OperationKind` type).
