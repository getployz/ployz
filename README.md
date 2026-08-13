<div align="center">
  <img src="assets/logo-wordmark.svg#gh-light-mode-only" alt="Ployz" height="72"/>
  <img src="assets/logo-wordmark-dark.svg#gh-dark-mode-only" alt="Ployz" height="72"/>
  <p><strong>A small-cluster orchestrator without a replicated core.</strong></p>
</div>

Ployz deploys containers across small clusters of cloud VMs and bare metal.
Every machine holds the cluster's Corrosion rows and serves HTTP/JSON/SSE over
a WireGuard mesh. Stock Docker remains execution reality.

One ordinary Corrosion row names an advisory preferred controller. Followers
forward cluster mutations to it, and it serializes them with one in-memory
lock. The controller is disposable: it has no lease, quorum, durable queue, or
workflow history, and interrupted work is retried from Corrosion and host
reality.

Each node embeds Microsoft Duroxide with private local SQLite only for bounded
prepare and retire effects on that node. Workflow history never becomes cluster
truth and never moves between machines.

## Current v2 scope

- Deploy or update the sole service in a namespace from a prebuilt registry
  image.
- Place replicated or global containers, gate newly created containers on
  health, and publish routes after preparation.
- Retain failed candidates for inspection and expose coarse deploy operation
  snapshots: created, then terminal.
- Serve internal DNS, public gateway routes, service logs, machine membership,
  and the WireGuard substrate.

Ployz v2 does not currently parse Compose files or build source images. It also
accepts split controllers during partitions: each machine acts from its local
Corrosion view, with no majority quorum or fencing protocol.

## Runtime shape

One `ployzd` artifact runs separately supervised Keeper, API, Gateway, and DNS
roles beside Docker and a version-pinned Corrosion sidecar:

```text
CLI / SDK / Cloud
       |
       v
any API node -> preferred controller -> target node Duroxide -> Docker
       |                |
       +------ Corrosion rows on every machine ------+
```

Keeper converges machine substrate toward recorded operator decisions; it does
not author new cluster intent. Ployz Cloud is an ordinary mesh peer and API
consumer, not runtime authority.

## Project status

Pre-1.0 and under active development; expect breaking changes. The incumbent
`v0.0.2` line is frozen. Coreless v2 ships as `v0.1.0-alpha.N`.

## Contributing

Start with the [contributor code map](docs/architecture/code-map.md), then read
[VISION.md](VISION.md), [CONTEXT.md](CONTEXT.md), the accepted
[ADRs](docs/adr/), and [AGENTS.md](AGENTS.md).
