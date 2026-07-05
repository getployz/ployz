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
not erase the scene. A stale machine should be visible as stale, not silently
converted into truth. Logs are evidence; operation status is the audience.

## Architecture Shape

Ployz is one daemon, one NATS control domain, and local runtime execution.

```text
CLI / SDK / Cloud
  -> NATS services
  -> operation workers
  -> machine services
  -> Docker / gateway / DNS / local machine reality
```

The control plane uses NATS primitives directly:

- Service API for commands.
- Plain subjects for fact broadcasts, intent broadcasts, service calls, and
  live operation progress.
- Core-local intent and evidence files for durable control-plane storage.
- Machine-local fact ledgers for machine-owned truth.
- RPC artifact push for deploy bundles, diagnostics, rendered specs, and cert
  material that machines need.
- Core-local timers that create explicit operations for delayed or recurring
  work.
- Subject permissions for authority.

Machines reach the control plane through direct TLS-authenticated NATS by
default. NATS is the command and state surface.

```text
machine async-nats
  -> TLS NATS
  -> nats-server
```

Product behavior is expressed in NATS subjects, messages, local evidence files,
and service handlers. Private overlay transport may be revisited later, but it
is not part of the v1 control-plane connection.

## State Model

Docker is execution reality.

Machines broadcast facts from Docker and their local fact ledgers.

The core owns operator intent in local evidence files and broadcasts it.

Operation evidence is local to the core and mortal with it unless an external
subscriber, such as Cloud, stores durable history.

RPC artifact push moves larger control-plane artifacts to the machines that use
them.

Each machine keeps a local fact ledger of durable machine-owned facts: route
attachments applied there, served certificate material, assigned substrate
state, last-known-good projections. The ledger is machine truth, never
cluster truth; the cluster view is assembled from machine facts and Docker
reality, which is what makes the core rebuildable.

Machine-local storage outside the fact ledger is a cache and evidence surface,
not truth of any kind.

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
Recovering a lost core is bounded core promotion plus fresh machine fact
broadcasts; preserved or restored intent evidence is adopted, while lost
intent must be re-entered rather than inferred.

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

The hard infrastructure behavior should come from NATS subjects, service calls,
and permissions. Ployz should not build custom versions of service discovery,
job engines, progress fanout, current state fanout, or permission routing.

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
not orchestrate low-level machine work.
