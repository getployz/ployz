<div align="center">
  <img src="assets/logo-wordmark.svg#gh-light-mode-only" alt="Ployz" height="72"/>
  <img src="assets/logo-wordmark-dark.svg#gh-dark-mode-only" alt="Ployz" height="72"/>
  <p><strong>Run a small cluster through explicit operations — every change owned, watchable, and safe to retry.</strong></p>
</div>

Ployz is a dead-simple, mesh-native control plane for deploying containerised
services across small clusters of cloud VMs and bare metal. Every mutating
action is an explicit, bounded operation with durable summary evidence and one
terminal result.

There is no core, quorum, sequencer, or message broker. Cluster configuration is
rows in a shared Corrosion store converged to every machine. One `ployzd` binary
runs separately supervised Keeper, API, Gateway, and DNS roles over a pluggable
WireGuard mesh; callers use HTTP/JSON and SSE.

Keeper is the narrow exception to the no-hidden-policy rule: it converges each
machine's substrate toward decisions an operator already recorded, and never
authors new cluster truth. Docker remains execution reality. Ployz Cloud is an
ordinary mesh peer and API consumer, not runtime authority.

## Development status

The coreless v2 line is being built from the accepted design in
[ADR 0040](docs/adr/0040-corrosion-replaces-the-core-and-nats.md). The workspace
currently exposes the six-crate topology and transport-free mechanics that the
Corrosion, HTTP, mesh, Keeper, and command slices build upon. There is no
supported quick-start path until those slices land.

## Features

- **Explicit operations, not magic** — every mutation returns an operation id with live progress, a typed result, and
  inspectable evidence.
- **Docker Compose in** — the familiar [Compose](https://compose-spec.io/) format, no bespoke DSL.
- **Zero-downtime deploys** — phase-ordered `start-first` rollouts: new containers pass their health gate before old
  ones stop.
- **Failures leave the scene intact** — a failed deploy retains its containers, logs, and typed reasons; retrying never
  erases prior failure.
- **Coreless control plane** — every machine holds the shared Corrosion rows;
  commands use HTTP/JSON and watches use SSE over the mesh.
- **Converged beats coordinated** — losing any one machine never blocks
  commanding the survivors; row writers and timestamps expose the accepted LWW
  trade-off.
- **Built-in gateway** — routes externally managed hostnames to your service containers; Ployz DNS resolves internal service names.

## Why Ployz?

Kubernetes reconciles the world behind your back — something drifts, a loop rewrites it, and you reverse-engineer *why*
from logs. Hand-rolled scripts leave no durable trail. Eventually-consistent meshes merge conflicting writes silently.
Ployz takes the pragmatic middle for those of us not running at Google scale:

- **Change has an owner** — every difference traces to an operation with an id, progress, and a result. "Why did this
  change?" is always answerable.
- **Debug the failure, not the tool** — failed containers, logs, and typed reasons are retained on purpose.
- **Simple as you grow** — start with one machine, add more with a single operation. No HA control plane to babysit.
- **Recover without heroics** — every surviving machine still holds cluster
  configuration; there is no control-plane promotion drill.

## How it works

One `ployzd` artifact runs as separately supervised roles beside stock Docker
and a version-pinned Corrosion sidecar:

```text
CLI / SDK / Cloud mesh peers
  -> HTTP/JSON + SSE over WireGuard
  -> Corrosion rows on every machine
  -> API fold + Keeper + Gateway + DNS
  -> Docker and machine-local substrate
```

Every mutating command runs the same lifecycle — no generic engine, no actor framework, no hidden reconciler:

```text
accepted -> planning -> running -> waiting_for_health -> completed
                                                     \-> failed (typed details + retained evidence)
                                                     \-> cancelled
```

Docker is execution reality. Config rows are operator decisions; status rows are
machine testimony; operation detail is driver-local evidence. Corrosion
subscriptions wake readers, which re-query the authoritative rows rather than
applying deltas as truth.

## Project status

Pre-1.0 and under active development; expect breaking changes. The incumbent
`v0.0.2` line is frozen; coreless v2 ships as `v0.1.0-alpha.N` once its release
acceptance ticket is complete.

## Contributing

Start with the [contributor code map](docs/architecture/code-map.md). It explains
the supervised runtime roles, state and dependency ownership, canonical module
boundaries, where each kind of change belongs, and which test level proves it.

Then read [VISION.md](VISION.md), [CONTEXT.md](CONTEXT.md), the accepted
[ADRs](docs/adr/), and [AGENTS.md](AGENTS.md) before changing product or
architecture behavior.
