<div align="center">
  <img src="assets/logo-wordmark.svg#gh-light-mode-only" alt="Ployz" height="72"/>
  <img src="assets/logo-wordmark-dark.svg#gh-dark-mode-only" alt="Ployz" height="72"/>
  <p><strong>Run a small cluster through explicit operations — every change owned, watchable, and safe to retry.</strong></p>
</div>

Ployz is a small-cluster orchestration core for deploying containerised services across cloud VMs and bare metal. Every
mutating action — add a machine, deploy a service, drain capacity — is an explicit **operation**: it returns an id,
streams durable progress, ends in one terminal result, and leaves readable evidence when it fails.

Nothing reconciles behind your back. No hidden control loop converging on a desired state, no eventually-consistent
store merging truth silently — if the cluster changed, an operation caused it. The control plane is a single
**disposable core** speaking NATS; machines own their runtime truth, so losing the core is a bounded recovery, not a
lost cluster.

The core stands alone, but it is built to be driven. Typed, bounded, watchable operations make an ideal API for SDKs,
AI agents, and **Ployz Cloud** — the cloud product this core is designed to power. What a person types at the CLI, an
agent can call with the same guarantees.

## Quick start

1. Bootstrap your first machine:

   ```bash
   curl -fsSL https://ployz.sh | sh && sudo ployz-keeper bootstrap
   ```

2. Add more machines:

   ```bash
   ployzctl machine add root@another-server-ip
   ```

3. Deploy from a Compose file (routes are declared in the file via `x-route`):

   ```bash
   ployzctl deploy -f compose.yaml -n myapp
   ```

4. Inspect what's running:

   ```bash
   ployzctl ls
   ployzctl ops list
   ```

Once DNS points a routed hostname at a machine, the gateway serves it. Lifecycle is operations too: `machine drain`,
`machine resume`, `ops watch <id>`, `inspect`.

## Features

- **Explicit operations, not magic** — every mutation returns an operation id with live progress, a typed result, and
  inspectable evidence.
- **Docker Compose in** — the familiar [Compose](https://compose-spec.io/) format, no bespoke DSL.
- **Zero-downtime deploys** — phase-ordered `start-first` rollouts: new containers pass their health gate before old
  ones stop.
- **Failures leave the scene intact** — a failed deploy retains its containers, logs, and typed reasons; retrying never
  erases prior failure.
- **NATS-native control plane** — commands, machine RPC, and live testimony on the NATS Service API; no custom job
  engine or progress bus.
- **Disposable core** — machines own runtime truth via local fact ledgers, so a lost core is promoted from an existing
  machine, not restored from a consensus database.
- **Built-in gateway** — routes external hostnames to your service containers, with DNS.

## Why Ployz?

Kubernetes reconciles the world behind your back — something drifts, a loop rewrites it, and you reverse-engineer *why*
from logs. Hand-rolled scripts leave no durable trail. Eventually-consistent meshes merge conflicting writes silently.
Ployz takes the pragmatic middle for those of us not running at Google scale:

- **Change has an owner** — every difference traces to an operation with an id, progress, and a result. "Why did this
  change?" is always answerable.
- **Debug the failure, not the tool** — failed containers, logs, and typed reasons are retained on purpose.
- **Simple as you grow** — start with one machine, add more with a single operation. No HA control plane to babysit.
- **Recover without heroics** — a lost core is bounded promotion plus fresh machine facts.

## How it works

One daemon, one NATS control domain, local runtime execution:

```text
CLI / SDK / Cloud
  -> NATS services      (commands, machine RPC, live testimony)
  -> operation workers  (validate, plan, run bounded work)
  -> machine services
  -> Docker / gateway / DNS
```

Every mutating command runs the same lifecycle — no generic engine, no actor framework, no hidden reconciler:

```text
accepted -> planning -> running -> waiting_for_health -> completed
                                                     \-> failed (typed details + retained evidence)
                                                     \-> cancelled
```

Docker is execution reality. Machines broadcast facts from Docker and local ledgers; the core owns operator intent in
local evidence files. The cluster view is assembled from machine facts and Docker reality, which is what makes the core
rebuildable. Ployz Cloud is a consumer of the core, not its owner: it drives operations and watches their events,
while the core stays the runtime authority.

## Project status

Pre-1.0 and under active development; expect breaking changes. Bootstrap resolves the `alpha` channel to an exact,
SHA-256-verified GitHub release and installs only `ployz-keeper`. Ployz never tracks GitHub `latest`.
