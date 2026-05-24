# Vision

Ployz is a primitive orchestration core for small clusters.

It turns common infrastructure operations into explicit commands: add
capacity, deploy workloads, move state, branch environments, promote, roll
back, and remove machines. Each command should have visible preconditions, a
bounded effect, a clear result, and a way to verify what happened.

Modern orchestrators — Kubernetes and the platforms built on top of it —
are complex declarative systems that scale to the moon by encoding policy
into the cluster itself. Controllers, operators, autoscalers, custom
resources, admission webhooks: all of these exist to manage fleets too large
for an operator to reason about one decision at a time.

That tradeoff is right for hyperscale infrastructure. It is the wrong default
for small clusters. Most teams do not need a cluster that constantly rewrites
itself toward a standing desired state. They need a system with strong
operational primitives: commands that are easy to inspect, safe to retry, and
honest when they cannot complete.

This changes the design problem. Instead of hiding operational work behind
policy engines, ployz makes the work legible and command-shaped. The cluster
gets smaller, more boring, and more predictable. Operators get more capable
because they have primitives instead of policies.

The bet: small-scale infrastructure gets better when the system exposes real
operational primitives instead of hiding them behind controllers and
reconciler loops. The same primitives that make a CLI good for humans also
make it usable by agents: concrete actions over a legible system, one
operation at a time.

## What This Means in Practice

- **No autoscalers, no controllers, no reconcilers.** The cluster does not
  change its own state in the background. Every state change is an explicit
  operation, triggered by an operator, with a clear return value.
- **Modest scale.** Ployz targets clusters in the 1–200 node range. That is
  the range an operator can reason about end-to-end, and it covers most real
  workloads. We do not pretend to compete with Kubernetes for 10,000-node
  fleets.
- **Primitives, not policies.** The product is a set of single-command
  operations — add a machine, migrate a workload, branch an environment,
  roll back a deploy — that compose into any workflow the operator wants.
  Policy lives in the operator's head, not in cluster manifests.
- **Live state, not desired state.** The system reports what is true now,
  on demand. There is no "desired state" sitting in storage waiting to be
  reconciled. Intent is captured at the moment of action and then it is
  done.

## The Operator Loop

The operator sees the cluster, decides what to do, runs a command, sees the
result, decides the next thing. That is the whole loop. There is no
controller running ahead of them or behind them. There is no manifest to keep
in sync.

Commands are evaluated from the perspective of the node the operator reached.
That node reads the replicated rows it has, probes any peers the operation
depends on, computes a concrete plan, and runs that plan. The system does not
require a globally perfect cluster view before every operation. Most durable
rows are owned by one actor and change rarely; if a partition or stale row
makes the operation unsafe, the command fails with a visible reason.

For a human: this means transparency. You can explain why the system
believes what it believes with a short causal chain. There are no surprise
mutations and no out-of-band convergence.

For automation and agents: this means tractability. They can plan, execute,
and verify in tight cycles. They do not have to predict reconciler behavior,
wait for eventual consistency, or reason about hidden state machines mutating
things underneath them.

Self-management is real, but it lives in the operator, not in the cluster.

## North Star

A user, or any tool acting on their behalf, should be able to:

- add a new machine to the cluster,
- migrate a workload to it,
- branch an environment for a PR,
- promote a branch to production,
- roll back if something is wrong,
- and remove a failed or unwanted machine,

each as one command, each completing or failing cleanly, each safe to retry.

The same model should work on a single developer Mac, on a small office
mesh, and on a fleet of bare-metal machines. The primitives are identical;
only the scale differs.

## What This Project Is

This project is the orchestrator core that exposes those primitives:

- the daemon,
- the deploy and branch model,
- the migration and transfer protocol,
- the runtime state model,
- the cluster coordination mechanisms,
- and the SDK and API surface that other interfaces — including CLI, agents,
  and ployz-cloud — rely on.

This core must stand on its own. Cloud, CLI, and automation loops are all
consumers of these primitives, not the source of truth for them.

## What This Project Is Not

Ployz is not Kubernetes. It is not a reconciler with a friendlier UI on
top. It is not a managed PaaS. It is not a configurable toolkit you
assemble into an orchestrator. It is not a place to expose every underlying
knob in the name of flexibility. It does not target hyperscale.

When a strong opinion makes the system better, simpler, or more capable, we
prefer the opinion. When a knob would let the user assemble something the
platform should have shipped as a primitive, we ship the primitive. When a
choice would add complexity to the cluster to handle a case the operator
could handle directly, we let the operator handle it.

## Relationship to ployz-cloud

Ployz-cloud is an optional, paid hosted product built on top of this
project: a Railway-style UI with a built-in operator that drives the cluster
on the user's behalf. It adds the niceties expected of a managed PaaS —
git-push deploys, environment dashboards, hosted machine pools, secrets
management, billing — and the hosted operator is the primary control surface
inside it. The cloud is where the commercial product lives.

This project is the open core. It is fully usable without ployz-cloud:
self-host on your own machines, drive it with the CLI or with any
general-purpose coding agent (Claude Code, Cursor, etc.), run `ployzctl dev`
locally, never pay anyone. The cloud exists for people who want the
managed experience and hosted operation; the core exists for everyone.

Ployz-cloud is a lens over this project. Every operation it exposes —
every deploy, every branch, every migration — is implemented as a
primitive shipped here. The cloud UI does not extend the cluster with
private mechanisms. It does not maintain its own model of cluster truth.
It does not add reconcilers or controllers that ployz core does not have.
If the cloud needs a capability, the right answer is almost always to
strengthen the primitives in this repo — both because it keeps the
architecture clean, and because anything cloud-specific in the core is a
tax on the open-source users who are not paying for the cloud.

The dependency is one-way. This project does not know about ployz-cloud,
does not assume the cloud is the operator, and does not optimize for the
cloud's UI flows. The bet is that if these primitives are great, ployz-cloud
— and any other downstream consumer, including general-purpose agents driving
the CLI directly — is great as a consequence.

The same primitives drive `ployzctl dev` on a developer's Mac, the cloud's
hosted environments, and any future on-prem deployment. One model, three
deployment shapes.

## The Primitive Surface

The operations below define the product. Each is a single command in the
CLI, with matching SDK and structured external surface. None rely on a
background reconciler to "eventually" complete.

- **`ployzctl machine add`** — provision a fresh machine into the cluster.
- **`ployzctl machine remove`** — drain workloads off a machine, transfer
  their state, take it out of the cluster.
- **`ployzctl migrate <workload> --to <machine>`** — move a workload,
  including its persistent state, between machines.
- **`ployzctl branch <env>`** — fork an environment, including its full state
  (datasets, volumes, secrets, routing), as a single atomic operation.
- **`ployzctl promote <branch>`** — atomically switch traffic from one
  environment to another. Old environment remains snapshotted for rollback.
- **`ployzctl rollback`** — restore the previous deploy point, including
  state.
- **`ployzctl fork-volume`** — clone a volume (e.g. a database) for use by
  another workload. Instant, copy-on-write.
- **`ployzctl dev`** — run the same model locally on a developer machine,
  with the same primitives.

If a user finds themselves writing a script to compose multiple ployzctl
commands to achieve a workflow, that workflow is a missing primitive.

## Core Beliefs

### 1. Simplicity in the cluster, intelligence in the operator

The cluster should be small enough that an operator can hold its model in
working memory. Every feature that adds in-cluster complexity to enable a
behavior the operator could enable directly is a feature against the bet.

### 2. Operation surfaces are first-class

The CLI and SDK are designed around primitive operations, not around a human
typing shell text by hand. Idempotent operations, structured output, typed
failures, visible preconditions, and explicit verification hooks are part of
the product surface. This makes the system better for humans and naturally
usable by agents, cloud workflows, and future automation.

### 3. The data plane outlives the control plane

The daemon (ployzd) is disposable. It can crash, upgrade, or restart
without disrupting WireGuard, iroh connectivity, the gateway, DNS, or workloads.
On startup it adopts what is already running rather than recreating it.
The daemon misbehaving must not brick the data plane.

### 4. Owning the substrate is the unlock

Ployz owns storage, network, and runtime end-to-end. ZFS gives instant
clone, atomic snapshot, and incremental send. WireGuard gives a flat
controllable network. Docker gives container identity we control. A
managed PaaS abstracts these away to serve many customers; we keep them,
because owning them is what makes single-command primitives possible.

The substrate boundary is deliberate. `polis` owns distributed substrate
primitives: replicated store access, transactions, subscriptions, change
cursors, endpoint identity, tickets, peer RPC, deadlines, probes,
membership records, and distributed failure typing. `ployz` owns product
behavior: machine join semantics, deploy semantics, namespace meaning,
routing decisions, capacity policy, volume movement, readiness, and operation
outcomes.

That means ordinary Ployz modules stay readable by depending on product
ports, while Ployz adapters translate those ports into Polis primitives.
Polis must not become a second Ployz backend with product-shaped APIs such
as deploy, routing, capacity, or machine-join policy. It may be
Corrosion-specific internally; the important abstraction is hiding
distributed mechanics from product code, not hiding Corrosion from Polis.

### 5. Operations are atomic

Every operation succeeds or fails clearly. Half-applied state presented as
success is the worst possible outcome because every downstream operator will
confidently move forward from a falsely-green signal. Loud failure beats
ambiguous progress.

### 6. Every node is a peer

There is no master. No special node holds state others lack. Coordination,
locking, and state visibility work on a peer-oriented model with iroh as the
foundational transport. This is what makes `machine remove` safe regardless of
which machine is removed.

### 7. ZFS is product strategy, not implementation detail

Branching, snapshotting, cloning, sending, and rolling back are the
substrate. ZFS is the primary backend because it makes those operations
cheap. Btrfs is supported as a small-machine tier with explicit migration
paths to ZFS. Storage capabilities are visible in the product surface, not
hidden behind a generic abstraction.

### 8. Live state matters more than projections

Durable cluster state represents operator intent and explicit lifecycle
events, not inferred health. Health and reachability are observed live at
decision time, when the operator asks. The system does not rewrite cluster
truth in the background from stale observations.

### 9. Corrosion rows are not command execution

Corrosion is the intended replicated state substrate. It carries cluster rows,
operation records, membership, placement inputs, and observations that peers
need to see. It is not the command bus, and it is not treated as a linearizable
source of truth for in-flight operations.

Mutating primitives still execute through bounded daemon-to-daemon RPC. The
coordinating node reads the replicated rows it currently has, checks live
preconditions for the peers involved, issues narrow internal RPCs for concrete
work, and records the outcome back into Corrosion rows. External API and CLI
requests must not be forwarded directly as peer RPC payloads; internal peer
RPC has its own typed protocol with only the operations a node is allowed to
perform for another node.

Anything replicated through the cluster — including TLS private keys, ACME
account keys, invite tokens, operation records, and placement facts — must be
treated as cluster-private material. For now, nodes with `storage=true` are
trusted with the full control-plane store; nodes with `storage=false` should
receive only the state they need for their runtime role.

The consequences follow from that:

- Storage-enabled nodes must be treated as trusted with the cluster's secrets.
- Store data-directory backups contain private key material in effect at the
  time of the backup unless encryption-at-rest says otherwise.
- Recovering from a suspected compromise means rotating the affected
  material (re-issuing certs, revoking ACME accounts), not just removing the
  machine.

If a future workload needs a stricter boundary, model that as scoped fact
replication and role-specific distribution, not as an ad hoc privacy flag on a
store record.

### 10. Local and cloud share one model

A developer running `ployzctl dev` on a Mac gets the same primitives as a
fleet operator. Branching, migration, rollback all work the same way. The
model does not bifurcate between "dev mode" and "real mode."

## Operator Experience Goals

Ployz should feel:

- fast to bootstrap (under five minutes from fresh box to working
  environment, no manual intervention),
- safe to automate,
- honest about which operations are automation-safe today versus which still
  require a human,
- predictable: the same command produces the same effect, with no hidden
  background change in the meantime,
- and easy to diagnose when something goes wrong.

The CLI is strong, but it is not the final product surface. The same
primitives drive a CLI, a hosted dashboard, and future automation.

## Design Standard

When making design decisions, prefer:

- a simpler cluster over a more capable one,
- intelligence in the operator over intelligence in the cluster,
- imperative commands over declarative reconcilers,
- one command over a sequence,
- a primitive over a documented procedure,
- atomicity over ambiguous progress,
- clear failure over silent correction,
- live observation at decision time over stored projections,
- API shapes that structured tools can drive by construction,
- one model across local and cloud over separate systems,
- and primitives that compose into future products.

If a feature requires a background reconciler to be correct, redesign so
it doesn't. If a feature requires the user to read a tutorial to use
safely, the feature is not done. If a feature adds in-cluster complexity
to handle a decision the operator could make directly, push the decision
to the operator instead.
