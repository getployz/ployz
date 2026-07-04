# Machine Usability View + drain/resume

## Context

Two independent architecture reviews derived the same gap: CONTEXT.md
defines the Machine Usability View ("so deploy, gateway, and machine
APIs do not each reimplement the rules") but the code has four scattered
variants — gateway has typed 30s freshness, the machine API has none
(stale container counts render as current), deploy facts have a third
shape, DNS a fourth. VISION's next unshipped verb is "drain capacity."
This slice ships both: the view as ~100 lines of pure core functions,
and drain/resume as its first lifecycle consumer.

Grilled decisions:

- Drain sets operator intent only: the machine leaves *new placement*
  immediately; running workloads keep serving and move off on the next
  deploy (ployz does not re-render manifests; no auto-migration).
- Serving eligibility stays observation-driven; lifecycle does not
  remove a draining machine's existing upstreams. Cleanup reachability
  is untouched — a draining machine must accept removals to empty.
- Paired operations: `machine drain` / `machine resume`, one
  `MachineLifecycle` operation kind, target lifecycle in the payload,
  `Accepted → Completed | Failed`. Idempotent: submitting the current
  lifecycle completes trivially.
- Observation cadence flips to the dumb drumbeat: machines publish
  every 30s; one shared stale-after of 90s (three missed beats)
  becomes the single freshness rule for gateway, DNS, machine API, and
  the view. (Uncloud-style: each machine owns and reports its own
  reality; consumers fold dumb rules over the pool.)
- Rebuild test (ADR 0001/0016): lifecycle is control-side durable
  authority with on-disk evidence — a machine-lifecycles JSON file
  written atomically before the KV projection, adopted into KV on
  control start exactly like `authorized-users.conf`. Machine-local
  commit was rejected because you drain unreachable machines; intent
  about a machine is not a machine-owned fact. SQLite consolidation is
  deliberately deferred to the ADR-0018 ledger work.
- v1 reasons are the three with real signals: `Draining`,
  `StaleObservation`, `NoRuntimeObservation`. The glossary's remaining
  reasons land with their signals.
- A deploy with zero usable machines fails with a typed
  `NoUsableMachines` failure carrying per-machine reasons.

## Changes

### 1. ployz-core: lifecycle + the view

- `MachineLifecycle { Active, Draining }` on `ActiveMachineState`
  (`#[serde(default)]` = `Active` so existing records decode).
- `ops::usability` (or `machine_usability.rs`): pure module with
  - `OBSERVATION_PUBLISH_INTERVAL` (30s) and
    `OBSERVATION_STALE_AFTER` (90s) as the shared constants;
  - `MachineUsabilityReason { Draining, StaleObservation,
    NoRuntimeObservation }`;
  - `machine_usability(lifecycle, last_observed_at, now) ->
    MachineUsability` where `MachineUsability` carries three typed
    verdicts: `placement`, `serving`, `cleanup`, each
    `Usable | Unusable { reason }`. Placement is lifecycle- and
    freshness-driven; serving is freshness-driven only; cleanup is
    unusable only on `NoRuntimeObservation`.
  - Table-driven unit tests; this is the one place the rules live.

### 2. The MachineLifecycle operation

- `OperationKind::MachineLifecycle`; submitted event
  `MachineLifecycleSubmitted { operation_id, machine_id, target }`;
  terminal events completed/failed. Subject + message-id arms on
  `OperationEvent` (terminal id `machine.lifecycle.terminal.{op}`),
  golden pins extended.
- Status/classification/projection arms per the machine-update
  pattern; minimal state machine.
- Endpoints `MachineDrain` / `MachineResume` on `OperationApiEndpoint`
  (both submit the one kind); ployzctl `machine drain <id>` /
  `machine resume <id>`; SDK types + TS regen.
- Worker: validate machine exists → write the evidence file → commit
  lifecycle to the KV machine record (projection after evidence, per
  ADR 0018's ordering) → record completed. Failure leaves typed
  evidence.

### 3. Lifecycle evidence file + adoption

- `machine-lifecycles.json` beside `authorized-users.conf`: one JSON
  document, atomic write, one writer (the lifecycle worker). Only
  non-default intent is recorded (draining machines); an absent entry
  means active.
- On control start, adopt file entries into KV before serving, exactly
  like `adopt_authorized_users_from_file`.

### 4. Consumers fold the view

- Deploy facts: `eligible_machines` filters by
  `usability.placement`; a deploy with zero usable machines fails
  `NoUsableMachines { reasons: Vec<(MachineId,
  MachineUsabilityReason)> }` (new typed deploy failure).
- Gateway source: `DEFAULT_GATEWAY_OBSERVATION_STALE_AFTER` (30s)
  replaced by the shared 90s constant; its private freshness enum
  folds into the view's serving verdict.
- DNS source: same substitution.
- Machine API: `MachineSnapshot` gains `lifecycle` and a typed
  `usability` summary (wire + TS addition) — a stale machine finally
  renders as stale; `ployzctl machine inspect` prints reasons.
- Machine observer: `MACHINE_OBSERVATION_INTERVAL` 1s → the shared
  30s constant (public-ip and gateway-status publishers align).

### 5. Docs

- ADR 0026: machine lifecycle intent is control-side durable authority
  (file evidence, adoption, the dead-machine-drain rationale, SQLite
  deferred to ADR 0018).
- CONTEXT.md already defines every term; add `Resume` if absent.

## Not in scope

- Auto-migration, hard eviction, machine remove.
- Dataplane-degraded / endpoint-subnet-mismatch / placement-constraint
  reasons (no signals yet).
- The ADR-0018 SQLite ledger.
- Cancellation (separate slice).

## Verification

1. View unit tests: table over (lifecycle × freshness) for all three
   verdicts.
2. Operation integration (NATS): drain → machine record shows
   draining + evidence file written → deploy places nothing there
   (or fails NoUsableMachines when it was the only machine) → resume →
   placement returns.
3. Adoption test: seeded lifecycle file → control start → KV shows
   draining, mirroring the authorized-users adoption test.
4. Golden pins extended for the new event family; wire-contract pin for
   the submitted event; TS regenerated and typechecked.
5. Gateway/DNS tests updated for the 90s constant; full suite green,
   zero warnings.
