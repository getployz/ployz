# Vision

Ployz is a simple cluster that an AI agent can reason about end-to-end and
manage with primitives.

Modern orchestrators — Kubernetes and the platforms built on top of it —
are complex declarative systems that scale to the moon by encoding policy
into the cluster itself. Controllers, operators, autoscalers, custom
resources, admission webhooks: all of these exist to fill in for a human
operator who cannot be in the loop for every decision. The cluster manages
itself, badly, so the SRE team only has to manage exceptions.

That tradeoff was correct when the operator was a human. It is the wrong
tradeoff when the operator is an AI agent. An agent *can* be in the loop
for every decision. It can hold the whole cluster in working memory, reason
about its current state, and choose the right command to run. The cluster
does not need to encode policy in advance because the agent decides policy
at decision time.

This inverts the design problem. Instead of a complex cluster that manages
itself, ployz is a simple cluster that an agent manages. The intelligence
moves from controllers in the cluster to the agent at the keyboard. The
cluster gets smaller, more boring, more predictable. The agent — and the
user — get more capable, because they have primitives instead of policies.

The bet: a simple cluster plus a capable operator beats a complex cluster
plus a passive operator. As agents get better, that gap widens.

## What This Means in Practice

- **No autoscalers, no controllers, no reconcilers.** The cluster does not
  change its own state in the background. Every state change is an explicit
  operation, triggered by an operator (human or agent), with a clear return
  value.
- **Modest scale.** Ployz targets clusters in the 1–200 node range. That is
  the range an agent can reason about end-to-end, and it covers most real
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

The operator (human or agent) sees the cluster, decides what to do, runs a
command, sees the result, decides the next thing. That is the whole loop.
There is no controller running ahead of them or behind them. There is no
manifest to keep in sync.

For a human: this means transparency. You can explain why the system
believes what it believes with a short causal chain. There are no surprise
mutations and no out-of-band convergence.

For an agent: this means tractability. The agent can plan, execute, and
verify in tight cycles. It does not have to predict reconciler behavior. It
does not have to wait for eventual consistency. It does not have to reason
about hidden state machines mutating things underneath it.

Self-management is real, but it lives in the operator, not in the cluster.

## North Star

A user, or an agent acting on their behalf, should be able to:

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
- and the SDK and API surface that other interfaces — including AI agents
  and ployz-cloud — rely on.

This core must stand on its own. Cloud, CLI, and agent loops are all
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
project: a Railway-style UI with a built-in agent (in the spirit of
Sentry's Seer) that drives the cluster on the user's behalf. It adds the
niceties expected of a managed PaaS — git-push deploys, environment
dashboards, hosted machine pools, secrets management, billing — and the
agent is the primary operator inside it. The cloud is where the commercial
product lives.

This project is the open core. It is fully usable without ployz-cloud:
self-host on your own machines, drive it with the CLI or with any
general-purpose coding agent (Claude Code, Cursor, etc.), run `ployzctl dev`
locally, never pay anyone. The cloud exists for people who want the
managed experience and the hosted agent; the core exists for everyone.

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
cloud's UI flows. The bet is that if these primitives are great,
ployz-cloud — and any other downstream consumer, including general-purpose
agents driving the CLI directly — is great as a consequence.

The same primitives drive `ployzctl dev` on a developer's Mac, the cloud's
hosted environments, and any future on-prem deployment. One model, three
deployment shapes.

## The Primitive Surface

The operations below define the product. Each is a single command in the
CLI, with matching SDK and agent-drivable surface. None rely on a
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

The cluster should be small enough that an agent can hold its model in
working memory. Every feature that adds in-cluster complexity to enable a
behavior the operator could enable directly is a feature against the bet.

### 2. The agent is a first-class operator

The CLI and SDK are designed assuming an AI agent is in the loop, not a
human typing commands. Idempotent operations, structured output, typed
failures. This applies whether the agent is the cloud's built-in operator,
a general-purpose coding agent driving the CLI, or a future on-prem
automation. The agent ranks above human ergonomic preference when the two
conflict — because a great agent surface is also, downstream, a great
human surface.

### 3. The data plane outlives the control plane

The daemon (ployzd) is disposable. It can crash, upgrade, or restart
without disrupting WireGuard, Corrosion, the gateway, DNS, or workloads.
On startup it adopts what is already running rather than recreating it.
This is what makes "an agent runs the cluster unattended" honest — the
daemon misbehaving cannot brick the data plane.

### 4. Owning the substrate is the unlock

Ployz owns storage, network, and runtime end-to-end. ZFS gives instant
clone, atomic snapshot, and incremental send. WireGuard gives a flat
controllable network. Docker gives container identity we control. A
managed PaaS abstracts these away to serve many customers; we keep them,
because owning them is what makes single-command primitives possible.

### 5. Operations are atomic

Every operation succeeds or fails clearly. Half-applied state presented as
success is the worst possible outcome, especially with an agent in the
loop, since the agent will confidently move forward from a falsely-green
signal. Loud failure beats ambiguous progress.

### 6. Every node is a peer

There is no master. No special node holds state others lack. Coordination,
locking, and state visibility work on a peer-oriented model with Corrosion
as a foundational part of the system. This is what makes `machine remove`
safe regardless of which machine is removed.

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

### 9. Mesh membership is the trust boundary

Corrosion replicates the full store to every mesh member. Anything written
to a replicated table — including TLS private keys, ACME account keys, and
invite tokens — lands on every machine's data directory in the same form it
was written. This is a deliberate design choice: it gives any machine the
ability to terminate TLS, serve routes, and take over control-plane
responsibilities without a separate key-distribution channel.

The consequences follow from that:

- Every mesh member must be treated as equally trusted with the cluster's
  secrets. There is no "gateway-only" node that holds less.
- Data-directory backups contain all private key material in effect at the
  time of the backup.
- Recovering from a suspected compromise means rotating the affected
  material (re-issuing certs, revoking ACME accounts), not just removing the
  machine.

If a future workload needs a stricter boundary than "any mesh member can read
it," that workload is outside the mesh's trust model and needs a separate
mechanism — not a privacy flag on a Corrosion table.

### 10. Local and cloud share one model

A developer running `ployzctl dev` on a Mac gets the same primitives as a
fleet operator. Branching, migration, rollback all work the same way. The
model does not bifurcate between "dev mode" and "real mode."

## Operator Experience Goals

Ployz should feel:

- fast to bootstrap (under five minutes from fresh box to working
  environment, no manual intervention),
- safe for an agent to drive,
- honest about which operations are agent-safe today versus which still
  require a human,
- predictable: the same command produces the same effect, with no hidden
  background change in the meantime,
- and easy to diagnose when something goes wrong.

The CLI is strong, but it is not the final product surface. The same
primitives drive a CLI, a hosted dashboard, and an agent loop.

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
- API shapes that an agent can drive by construction,
- one model across local and cloud over separate systems,
- and primitives that compose into future products.

If a feature requires a background reconciler to be correct, redesign so
it doesn't. If a feature requires the user to read a tutorial to use
safely, the feature is not done. If a feature adds in-cluster complexity
to handle a decision the operator could make directly, push the decision
to the operator instead.
