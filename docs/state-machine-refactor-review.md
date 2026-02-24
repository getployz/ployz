# State-Machine Refactor Review

This document assumes the refactor is complete.

Purpose:
- find holes in state transitions,
- identify simplifications that are now safe,
- measure whether the refactor actually improved the system,
- guide additional migrations without reintroducing old complexity.

## Architecture Snapshot (Post-Refactor)

Current model:
- single network per daemon,
- explicit phase types per stateful domain,
- durable phases only where lifecycle survives process restarts,
- lazy watch sources (first subscriber starts source, last subscriber stops source),
- string values at storage/API boundaries, typed enums internally.

Primary package ownership:
- `internal/network`: network runtime lifecycle and persisted runtime state.
- `internal/convergence`: worker lifecycle and reconcile orchestration.
- `internal/signal`: `freshness`, `ping`, `ntp`.
- `internal/watch`: topic broker, source lifecycle, replay/cursor support.
- `internal/deploy`: deployment lifecycle, ownership, tier execution.
- `internal/controlplane`: API/manager/proxy/serve lifecycle.
- `internal/store/local` and `internal/store/cluster`: sqlite and corrosion adapters.

## State Machine Inventory

Durable:
- `NetworkRuntimePhase` (network runtime record).
- `DeployPhase` (deployment record).

Ephemeral:
- `WorkerPhase`.
- `LoopPhase`.
- `SubscriptionPhase`.
- `CorrosionContainerPhase`.
- `FreshnessPhase`.
- `NTPPhase`.
- `PingPhase`.
- `OwnershipPhase`.
- `TierPhase`.
- `ServePhase`.
- `AddPhase`.

## Transition Hole Audit

Use this checklist for every phase type.

### 1. Topology holes

Check:
- zero value is invalid (`iota + 1`, parse rejects empty/unknown),
- every non-terminal phase has at least one outbound transition,
- every terminal phase is intentional and documented,
- no phase is unreachable from the machine start phase,
- no duplicate phases with identical semantics.

Red flags:
- phase exists but has no callers,
- transition edge exists but never fires,
- broad `default` branch allows hidden transitions.

### 2. Runtime holes

Check:
- every transition has one owner function,
- side effects occur in one phase and commit phase change after success,
- retries always move through an explicit retry/backoff phase,
- cancel/shutdown path transitions to a terminal or stable idle phase,
- fatal failures preserve reason (`lastErr`, `errorPhase`, `errorTier`, message).

Red flags:
- "stuck" in `Starting`/`Stopping`,
- running work while phase says stopped,
- repeated retries without phase updates.

### 3. Persistence holes (durable machines only)

Check:
- persisted value is string enum with strict parse,
- load path rejects unknown strings loudly,
- write path never writes empty/unknown phase,
- restart semantics are deterministic from persisted phase,
- durable phase and durable reason fields remain consistent.

Red flags:
- persisted phase not in parser,
- implicit fallback to old values,
- transient phases accidentally persisted.

### 4. Observability holes

Check:
- each critical transition emits a structured log/event,
- status APIs expose phase and reason separately,
- watch streams emit `resync`/`recovered` events after source recovery,
- health aggregation reads machine phase, not inferred booleans.

Red flags:
- only errors are logged, transitions are invisible,
- status output forces callers to infer state from mixed booleans.

### 5. Concurrency holes

Check:
- phase mutation is behind one lock owner per machine instance,
- watch source start/stop is reference-counted,
- no duplicate source goroutines per topic,
- no global in-memory mirrors when subscriber count is zero.

Red flags:
- source keeps running after last unsubscribe,
- two goroutines race transitions for same entity,
- replay ring grows without bound.

## Subsystem-Specific Hole Checks

### Network runtime

Must hold:
- `Running` means runtime setup completed, not just process alive,
- `Stopping` always converges to `ConfiguredStopped` or `Purged`,
- `Failed` is recoverable via explicit restart path.

Audit points:
- `internal/network/runtime/*`
- `internal/store/local/*` for durable phase serialization.

### Convergence worker + reconcile

Must hold:
- `WorkerDegraded` means worker still active,
- `WorkerBackoff` means no active reconcile loop,
- reconcile `Resubscribing` only after stream failure/close,
- channel close and context cancel are distinct termination reasons.

Audit points:
- `internal/convergence/engine/*`
- `internal/convergence/reconcile/*`

### Watch + subscription

Must hold:
- first subscriber starts source,
- last subscriber stops source,
- replay is bounded,
- old cursors trigger explicit `resync-required` instead of silent gaps.

Audit points:
- `internal/watch/*`
- `internal/store/cluster` subscription adapter.

### Deployment

Must hold:
- exactly one of `succeeded` or `failed` final states,
- ownership loss always fails or aborts apply,
- tier phase and deploy phase stay coherent,
- error reason fields survive finalization.

Audit points:
- `internal/deploy/*`

### Signals

Must hold:
- no sentinel RTT values (`PingPhase` carries reachability),
- no stale bool drift (`FreshnessPhase` is source of truth),
- NTP phase is explicit (`healthy`, `offset_unhealthy`, `error`, `unchecked`).

Audit points:
- `internal/signal/freshness/*`
- `internal/signal/ping/*`
- `internal/signal/ntp/*`

## Simplification Passes (After Stabilization)

Use these in order; stop when readability gains flatten.

1. Collapse near-duplicate phases
- If two adjacent phases have identical behavior and observability, merge them.

2. Remove proxy/mapper translation layers that only rename fields
- Prefer one canonical domain shape per topic.

3. Co-locate phase and transition trigger code
- Keep phase type and mutating methods in the same file when possible.

4. Keep one reason type per machine
- Example: `WorkerReason`, `DeployReason` structs instead of free-form strings across layers.

5. Reduce fan-out of status surfaces
- Expose machine phase, reason, and timestamp; derive booleans at UI edge only.

## Success Analysis Rubric

Track these in one table each release.

### A. Structural metrics

- production LoC (exclude tests/tools),
- number of top-level packages,
- number of method signatures carrying `network` routing key (target: 0 except config payload),
- number of status booleans that duplicate phase (target: minimal).

### B. Runtime metrics

- idle memory with zero watchers,
- goroutine count with zero watchers,
- per-topic source goroutines with N subscribers,
- watch recovery time after source interruption,
- worker restart/backoff frequency.

### C. Correctness signals

- percentage of invalid transition attempts (should be 0),
- percentage of watch reconnects requiring full resync,
- deploy completion ratio and average phase duration,
- count of "stuck phase > timeout threshold" incidents.

### D. Developer experience

- median time to add a new watch topic,
- median time to add a new deploy phase field,
- number of files touched for a typical lifecycle change.

## Additional Migrations To Consider

1. Unified watch proto/event schema
- one stream, topic-tagged payloads, consistent cursor semantics.

2. Reason code normalization
- replace free-form error strings with typed reason codes plus detail text.

3. Durable transition log (optional)
- append-only operational trace for deploy/network phases if debugging requires history.

4. Tighten package boundaries
- ensure only owner package can transition its phase type.

5. CLI simplification
- remove any residual cluster/network routing options that no longer matter with single-network daemon model.

## Review Cadence

Run this review at:
- +2 weeks after major architectural cuts,
- +6 weeks for simplification pass,
- each release where a new state machine or watch topic is added.

Exit criteria for "architecture stabilized":
- no unresolved transition holes,
- no persistent phase drift incidents,
- no idle watch-source leaks,
- no demand to reintroduce multi-network in-process routing.
