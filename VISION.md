# Vision

Ployz is a small-cluster orchestration core built around explicit operations.

It should make infrastructure work feel direct: initialize a cluster, add a
machine, deploy a service, inspect an operation, stream logs, drain capacity,
roll back, clean up. Each action should have visible inputs, durable progress,
a clear terminal result, and enough evidence to debug what happened.

The system is for the 1-200 machine range: homelabs, small teams, customer
owned servers, bare metal, and modest VPS fleets. At that scale, reliability
comes from simple mechanics and readable behavior, not from a large hidden
policy engine.

## Product Bet

Small infrastructure should be operated through primitives, not through a
cluster that constantly rewrites itself toward a standing desired state.

Ployz should give humans, agents, CLIs, SDKs, and cloud workflows the same
bounded operations:

- submit a command,
- get an operation id,
- watch durable progress,
- inspect current state,
- see the exact failure,
- retry or clean up deliberately.

The cluster should not surprise the operator. If something changes, an
operation caused it.

## Experience Goals

Ployz should feel:

- terse,
- observable,
- hard to accidentally misuse,
- safe to retry,
- honest when uncertain,
- easy to automate,
- small enough to hold in your head.

Failures are part of the product. A failed deploy should leave useful evidence,
not erase the scene. A stale node should be visible as stale, not silently
converted into truth. Logs are evidence; operation status is the audience.

## Architecture Shape

Ployz is one daemon, one NATS control domain, and local runtime execution.

```text
CLI / SDK / Cloud
  -> NATS services
  -> operation workers
  -> node services
  -> Docker / gateway / DNS / local machine reality
```

The control plane uses NATS primitives directly:

- Service API for commands.
- JetStream KV for current state.
- JetStream streams for operation history and job triggers.
- Durable consumers and queue groups for workers.
- Object Store for deploy bundles, diagnostics, rendered specs, cert material,
  and backup manifests.
- Message schedules for delayed or recurring work where available.
- Subject permissions for authority.

Machines reach the control plane through direct TLS-authenticated NATS by
default. NATS is the command and state surface.

```text
machine async-nats
  -> TLS NATS
  -> nats-server
```

Product behavior is expressed in NATS subjects, messages, KV records, streams,
and service handlers. Private overlay transport may be revisited later, but it
is not part of the v1 control-plane connection.

## State Model

Docker is execution reality.

NATS KV is current control-plane state.

NATS streams are durable timelines.

Object Store holds larger control-plane artifacts.

Local node storage is a cache and evidence surface, not cluster truth.

Operation state is first-class:

```text
accepted
planning
running
waiting_for_health
completed
failed
cancelled
```

Terminal failures should carry typed details: what failed, where it failed,
what was retained, and what the operator can do next.

## Consistency Thesis

Machines own their runtime truth. The control plane is one disposable core.
Nothing in the cluster runs consensus.

When the core is unreachable, operations fail loudly with typed errors and
the data plane keeps serving last-known-good state with visible freshness.
Recovering a lost core is a bounded reindex operation that rebuilds the
core's view from fresh machine facts, not quorum repair.

Loud unavailability always beats silently divergent truth. Ployz does not
adopt a consensus database, and it does not adopt a partition-tolerant store
whose writes merge silently. `docs/architecture/backbone.md` carries the full
thesis and its guardrails.

## Code Standard

Business logic should be extremely easy to read.

The deploy path should look like product policy:

```text
validate request
create operation
load current service state
acquire resource lock
plan changed containers
run predeploy
start replacements
wait for health
switch route
remove old containers
commit active revision
complete operation
```

The hard infrastructure behavior should come from NATS. Ployz should not build
custom versions of service discovery, job queues, progress streams, current
state fanout, or permission routing.

Prefer:

- plain structs,
- explicit enums,
- narrow modules,
- small async functions,
- typed ids,
- typed failures,
- obvious ownership boundaries.

Avoid:

- broad frameworks,
- generic operation engines,
- actor systems,
- hidden reconcilers,
- stringly state,
- sparse option bags,
- clever subject type algebra,
- background mutation without an operation owner.

## Cloud Relationship

Ployz Cloud is a consumer of the core.

Cloud owns product workflow state: organizations, projects, GitHub integration,
build records, billing, notifications, UI history, and long-running product
workflow orchestration.

The core owns runtime truth: machines, services, routes, certs, observations,
operation events, operation status, retained artifacts, and cleanup primitives.

Cloud should call small core operations and watch operation events. It should
not orchestrate low-level node work.
