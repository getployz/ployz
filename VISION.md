# Vision

Ployz exists to make small-to-mid-sized fleets feel easy to operate.

The target is operators running roughly 1 to 200 servers who want the power to
grow, replace, and rebalance machines without adopting the complexity of
Kubernetes. Adding capacity should be routine. Replacing a weak node with a
stronger one should be routine. A server should feel disposable, not precious.
That applies both to a single developer machine and to a real fleet in cloud.

Ployz is an opinionated orchestrator. That is intentional. We are not trying to
be a general-purpose platform for every infrastructure style. We choose hard
defaults when those defaults unlock better operational behavior.

## North Star

Ployz should make it trivial to:

- boot a new machine,
- join it to the cluster,
- move workloads onto it,
- and remove old machines with little or no downtime.

The long-term ideal is that cluster expansion feels almost automatic. A user
should be able to provision many machines and have them join cleanly with
minimal manual setup, eventually without requiring traditional SSH-driven
bootstrap flows.

Ployz should also scale down cleanly. A single macOS machine should be able to
run the same core model locally, with the ability to grow into a multi-machine
mesh by adding nearby hardware or cloud machines as needed.

## What This Project Is

This project is the orchestrator core:

- the daemon,
- the deployment model,
- the runtime state model,
- the cluster coordination mechanisms,
- and the SDK and API surface that other interfaces can rely on.

This core must stand on its own. The future cloud product is an interface over
this system, not the source of truth for it.

The same core should work well in three modes:

- on a single developer machine,
- on a small mixed local network,
- and on cloud infrastructure.

## What This Project Is Not

Ployz is not trying to become Kubernetes.
Ployz is not trying to become Docker Swarm.
Ployz is not trying to be infinitely configurable at the expense of strong
operational guarantees.

When a strong opinion makes the system better, simpler, or more capable, we
prefer the opinion.

## Core Product Beliefs

### 1. Machines should be easy to add

A cluster should be able to absorb new machines quickly and safely. Join flows,
adoption flows, network allocation, and cluster registration should be designed
for concurrency and low friction.

### 2. Deploys should be atomic in practice

A deploy should succeed or fail clearly. We do not want vague half-applied
states presented as success. More sophisticated rollout strategies can come
later, but the baseline contract is decisive and predictable change.

### 3. Every node should be a first-class participant

We design around a peer-oriented cluster, with Corrosion as a foundational part
of the system. Coordination, locking, and state visibility must work in a way
that preserves confidence in what the cluster believes to be true.

### 4. Opinionated storage unlocks better orchestration

We lean into ZFS as a platform assumption.

ZFS gives us:

- quotas as a default discipline,
- cheap snapshots,
- fast cloning,
- incremental replication,
- and practical workload migration with very low downtime.

This is not an incidental implementation detail. It is part of the product
strategy. By being opinionated here, we can make storage-heavy workloads, such
as databases, much easier to move and manage.

### 5. Live state matters more than projections

The system should expose real cluster state cleanly enough that future
products, including cloud, can act as a lens over that live state rather than
inventing a separate model of reality.

### 6. Local and cloud should share one model

Local development should not be a separate universe with separate assumptions.
The long-term goal is that a developer can run a `dev` command and boot an
environment that is fundamentally the same shape as the cloud environment, just
scaled down to local resources.

That means:

- the local workflow should reuse the same deployment and networking concepts,
- the same application topology should work on one Mac or many machines,
- and future cloud-aware tooling should be able to mirror cloud environments
  locally with minimal translation.

### 7. The SDK must make higher-level products easy to build

The SDK should expose the primitives needed for:

- deploy previews,
- applies,
- events,
- resource inspection,
- locking,
- machine adoption,
- local and remote environment mirroring,
- tunneling and gateway exposure,
- mixed process and container development workflows,
- and fast environment lifecycle management.

If cloud, agents, and future CLIs need a capability, the right answer is
usually to strengthen the core API or SDK rather than re-implement logic
elsewhere.

## Operator Experience Goals

Ployz should feel:

- fast to understand,
- fast to bootstrap,
- safe to operate,
- and pleasant to automate.

The CLI should be strong, but it is not the final product surface. The core
should support a great CLI, a great cloud dashboard, and great agent workflows
from the same underlying model.

Local development should feel first-class. A developer should be able to run a
realistic environment on a Mac, optionally attach additional nearby machines,
and expose the result through the same networking model that cloud uses.

That includes workflows where some components are normal containers and others
are local development processes such as `npm run dev`. Ployz should eventually
make those process-backed services feel native to the orchestrator, including
network routing, gateway exposure, and workflows such as sharing a live app on
another device while preserving hot reload behavior.

## PR Environments

Fast PR environments are a strategic capability.

The orchestrator should support the primitives that make this possible:

- fast cloning,
- environment-scoped overrides,
- reproducible deploy manifests,
- workload mobility,
- and storage semantics that make snapshot-based environments practical.

Much of the final UX may live in cloud, but the core system must make those
workflows natural rather than bolted on.

## Design Standard

When making design decisions, prefer:

- disposability over snowflakes,
- strong defaults over endless knobs,
- one model across local and cloud over separate systems,
- live truth over stale projections,
- atomicity over ambiguous progress,
- and primitives that compose into future products.

If a feature makes the system more generic but less coherent, it is probably
the wrong feature.
