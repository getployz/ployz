# Ployz

Ployz runs containerized Services across a Cluster of your own Docker Machines.
Cloud authors the Deploy Intent and drives the Cluster; the `ployz` CLI operates
the same Cluster over SSH contexts. Neither is a control plane.

| Project | Owns | Start here |
| --- | --- | --- |
| **Core** | Engine, `ployz` CLI, `ployzd` daemon, internal SDK, installer, releases | [core/README.md](core/README.md) |
| **Dashboard** | Ployz Cloud: web, worker, durable workflows, self-hosting | [dashboard/README.md](dashboard/README.md) |

```text
core/              Cargo workspace, installer, release scripts
dashboard/         pnpm application; self-host/ holds the Compose deployment
Dockerfile.cloud   Cloud runtime image, packaged by the Cloud CI job
scripts/           CI check selection
.github/           Workflows
docs/agents/       Agent guidance for domain changes
```

Run Cargo commands from `core/` and pnpm commands from `dashboard/`. Dashboard
links `@ployz/sdk` from `core/crates/ployz-sdk`; changes across the boundary land
together. Releases: [core/docs/RELEASE.md](core/docs/RELEASE.md).

Core is licensed under [Apache-2.0](core/LICENSE). Dashboard is licensed under
[AGPL-3.0](dashboard/LICENSE).

See the [context map](CONTEXT-MAP.md) for ownership and terminology.
