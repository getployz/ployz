# Agent Instructions

## Read First

- Read `VISION.md` before product or architecture work.
- Read `CONTEXT.md` before product, architecture, or domain-model work. Use
  its preferred terms in code, docs, tests, CLI copy, and operation/state names.
- Treat the current repository as a greenfield reset with an empty Rust
  workspace. Build the new shape deliberately.

## Product Direction

Ployz is a small-cluster orchestration core built around explicit operations.

Every mutating action should:

- validate preconditions,
- create an operation,
- emit durable progress,
- perform bounded work,
- finish with one terminal result,
- leave useful evidence on failure.

The product is primitives, not hidden policy. Do not add background behavior
that changes cluster truth without an operation owner.

## Architecture

Use NATS as the control-plane backplane:

- NATS Service API for commands/RPC.
- JetStream KV for current state.
- JetStream streams for operation history and durable job triggers.
- Durable consumers and queue groups for workers/retries.
- Object Store for larger control-plane artifacts.
- Message schedules for delayed/recurring work when supported.
- Subject permissions for authority.

Use direct TLS-authenticated NATS for machine control-plane connectivity:

```text
async-nats
  -> TLS NATS
  -> nats-server
```

Private overlay transport may be revisited later. Product commands go through
NATS.

## Control Plane And Data Plane

- `ployzd` is control plane: bootstrap, health, services, controllers, machine RPC.
- `ployzd` is not the data plane.
- `nats-server`, gateway, DNS, and workloads are independently supervised.
- Core `ployzd` down must not mean NATS/gateway/DNS down.
- Edge `ployzd` down stops that machine's RPC/observations, not its running
  workloads.
- Gateway and DNS watch NATS directly and keep last-known-good state.
- If `ployzd` starts data-plane/substrate processes, it is a supervisor and
  needs explicit readiness, restart, shutdown, health, and recovery tests.

## Module Ownership

Expected crate shape:

- `ployz-core`: ids, subjects, state models, operation models, deploy planning,
  security role models.
- `ployz-nats`: NATS connection, bootstrap, KV, streams, Object Store,
  services, schedules, permissions.
- `ployz-transport`: future transport adapters if private connectivity returns.
- `ployzd`: process wiring, service handlers, controllers, machine agent, Docker,
  gateway, DNS, certs.
- `ployzctl`: CLI client.
- `ployz-sdk-types`: public schema/type export surface.

Keep dependencies flowing inward. Business logic must not import process wiring.
Transport adapters must not import product orchestration convenience types.

## Control Plane Rules

- User-facing commands are NATS services.
- Machine-local commands are machine-scoped NATS services.
- Mutating services return operation ids quickly.
- Workers consume durable operation/job subjects.
- Queue groups distribute workers.
- KV locks are only for resource fencing.
- Subject permissions are the authority boundary.
- NATS credentials and subject permissions are the authority boundary.
- No external control-plane I/O may wait forever.
- Every long-running task needs shutdown, timeout, retry/backoff, and visible
  health.

## State Rules

- Docker is execution reality.
- Docker labels are recovery evidence.
- Local machine storage is cache/evidence, not cluster truth.
- KV stores current state.
- Streams store event history and job triggers.
- Object Store stores larger control-plane artifacts.
- Active service state is committed only after successful deploy completion.
- Pending and failed targets live in operation state/events.
- Do not infer liveness into stored truth.

## Operation Rules

- Model operation state as enums with explicit transitions.
- Terminal states are final.
- Failed operations carry typed failure details.
- Failed started deploy containers should be retained for inspection.
- Retrying must not erase prior failure.
- Logs are evidence, not the audience.
- Operation status and events are the audience.
- Next deploys may converge from observed reality, but background loops must
  not silently mutate cluster truth.

## Code Style

- Prefer plain structs, enums, and async functions.
- Add a trait only when there are two real implementations or a hard test seam.
- Avoid generic operation engines.
- Avoid actor frameworks.
- Avoid stringly states.
- Avoid sparse option bags for variant data.
- Encode system invariants in types. If a state, transition, target, or failure
  shape is invalid, make it unrepresentable instead of documenting the rule.
- Use typed ids for storage keys, subjects, placement, routing, authorization,
  and operation state.
- Keep handlers small. A handler must not own transport, authorization,
  orchestration, storage, and presentation at once.
- Centralize subject construction without building a complex type-level subject
  language.

## Rust Rules

- Use slice patterns over indexing.
- Use explicit enum values; avoid `Default::default()`.
- Destructure in trait impls to catch new fields.
- Match project enums exhaustively; no wildcard arms for convenience.
- Never `.unwrap()` optional state; use `let Some(x) = opt else { ... }`.
- Add `#[must_use]` to builder methods returning `Self`.
- Prefer enums over booleans for modes, phases, policies, outcomes, freshness,
  and failure classes.
- Prefer variant-specific data over optional fields shared across variants.
- Booleans are only for obvious yes/no facts with no plausible third state.
- Treat Clippy suppressions as a last resort; fix the shape first.
