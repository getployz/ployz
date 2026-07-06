<div align="center">
  <h1>Ployz</h1>
  <p><strong>Run a small cluster through explicit operations — every change owned, watchable, and safe to retry.</strong></p>
</div>

Ployz is a small-cluster orchestration core for deploying and operating containerised services across cloud VMs and
bare metal. It turns every mutating action — add a machine, deploy a service, drain capacity, clean up — into an
explicit **operation**: a command that returns an id, streams durable progress, ends in one terminal result, and leaves
readable evidence when it fails.

Unlike a reconciling orchestrator, nothing in the cluster rewrites itself behind your back. There is no hidden control
loop converging toward a standing desired state, and no eventually-consistent store merging conflicting truth silently.
If the cluster changed, an operation caused it — and you can point at the operation that did. The control plane is a
single **disposable core** speaking NATS; machines own their own runtime truth, so losing the core is a bounded
recovery, not a lost cluster.

Ployz is for the 1–200 machine range — homelabs, small teams, customer-owned servers, modest VPS fleets — where
reliability comes from simple mechanics and legible behaviour, not a large hidden policy engine. It gives humans,
agents, CLIs, SDKs, and cloud workflows the exact same bounded operations.

## Features

* **Explicit operations, not magic**: Every mutation returns an operation id with live progress, a typed terminal
  result, and evidence you can inspect. The cluster never surprises you.
* **Docker Compose in**: Define services with the familiar [Compose](https://compose-spec.io/) format. No bespoke DSL
  to learn; Ployz turns it into a planned deploy.
* **Zero-downtime deploys**: Phase-ordered rollouts with `start-first` replacement — new containers start and pass
  their health gate before old ones stop.
* **Failures leave the scene intact**: A failed deploy retains its stopped containers, logs, and typed failure
  details as evidence. Retrying never erases prior failure.
* **NATS-native control plane**: Commands, machine RPC, and live testimony run on the NATS Service API. No custom job
  engine, service discovery, or progress bus to operate.
* **Disposable core**: The control plane is one mortal core. Machines own their runtime truth via local fact ledgers,
  so a lost core is promoted from an existing machine — not restored from a fragile consensus database.
* **Built-in ingress**: A gateway serves your routes with TLS certificates and route-level DNS, plus route protection
  (public, password, or product-managed private) without touching the service.
* **Loud over silent**: When the core is unreachable, operations fail loudly with typed errors while the data plane
  keeps serving last-known-good state with visible freshness. Loud unavailability always beats silently divergent truth.
* **Made for automation and agents**: The same terse operations drive a human at a CLI, an SDK client, an agent, or
  Ployz Cloud. Everything is scriptable and honest when uncertain.

## Why Ployz?

Kubernetes gives you power and flexibility, but it reconciles the world behind your back: something drifts, a loop
rewrites it, and you reverse-engineer *why* from logs after the fact. Hand-rolled deploy scripts are legible but leave
no durable trail — a failed run is a scrollback you already lost. Eventually-consistent meshes keep working when
machines split, but they merge conflicting writes silently, so "the cluster state" is whatever the CRDT settled on.

There is a pragmatic middle for the majority of us who aren't running at Google scale. You should be able to:

* **Trust that change has an owner**: Every difference in your cluster traces to an operation with an id, a progress
  stream, and a terminal result. "Why did this change?" is always answerable.
* **Debug the failure, not the tool**: When a deploy fails, the failed containers, logs, and typed reasons are
  retained on purpose. Evidence is the product; logs are for reading, operation status is the audience.
* **Stay simple as you grow**: Start with one machine and add more with a single operation. No highly-available
  control plane to babysit, no quorum to keep alive.
* **Recover without heroics**: Machines own their runtime truth, so recovering a lost core is bounded promotion plus
  fresh machine facts — not a consensus-database restore and a prayer.

Ployz's goal is to make operating your own small cluster feel as direct and legible as running a single command —
whether that's on a $5 VPS, a spare Mac mini, or a rack of bare metal.

## Quick start

1. **Bootstrap your first machine** (installs `ployz-keeper` and forms the cluster):

   ```bash
   curl -fsSL https://ployz.sh | sh && sudo ployz-keeper bootstrap
   ```

   Or drive it from your workstation over SSH:

   ```bash
   ployzctl machine init root@your-server-ip
   ```

2. **Add more machines** to the cluster whenever you need them:

   ```bash
   ployzctl machine add root@another-server-ip
   ```

3. **Deploy your app** from a Compose file into a namespace, publishing a container port over HTTPS:

   ```bash
   ployzctl deploy -f compose.yaml -n myapp \
     --route-hostname app.example.com --route-port 443 --endpoint-port 8000
   ```

4. **Watch the operation** and inspect what's running:

   ```bash
   ployzctl ops watch          # live progress for the deploy operation
   ployzctl ls                 # list services
   ployzctl logs myapp         # stream service logs
   ```

5. Point an A record for `app.example.com` at your machine's public IP, give DNS a minute, and your app is live at
   https://app.example.com

Then explore capacity and lifecycle operations — `ployzctl machine drain`, `ployzctl machine resume`,
`ployzctl ops list`, `ployzctl inspect` — each one an explicit, retryable operation.

## How it works

Ployz is **one daemon, one NATS control domain, and local runtime execution**:

```text
CLI / SDK / Cloud
  -> NATS services        (commands, machine RPC, live testimony)
  -> operation workers    (validate, plan, run bounded work)
  -> machine services
  -> Docker / gateway / DNS / local machine reality
```

Every mutating command flows through the same shape — no generic operation engine, no actor framework, no hidden
reconciler:

```text
accepted -> planning -> running -> waiting_for_health -> completed
                                                      \-> failed  (typed details + retained evidence)
                                                      \-> cancelled
```

**Docker is execution reality.** Machines broadcast facts from Docker and their local fact ledgers; the core owns
operator intent in local evidence files and broadcasts changes; operation evidence is the core's append-only log,
mortal with it unless an external subscriber (such as Ployz Cloud) keeps durable history. The cluster view is assembled
from machine facts and Docker reality, which is what makes the core rebuildable.

Ployz Cloud is a *consumer* of the core, not its owner: Cloud holds product workflow — organizations, projects, GitHub
integration, builds, billing, history — and calls small core operations while watching their events. The core owns
runtime truth.

## Project status

Ployz is pre-1.0 and under active development, so expect breaking changes between releases. Bootstrap resolves the
`alpha` channel to an exact GitHub release, verifies its SHA-256, and installs only `ployz-keeper`; pin an exact
version when reproducibility matters:

```bash
curl -fsSL https://ployz.sh | sh -s -- --version v0.0.1-alpha.1 && sudo ployz-keeper bootstrap
```

Ployz does not use GitHub `latest`. Release publishing and channel promotion are documented in
[`docs/operations/release.md`](docs/operations/release.md).
