# OpenShip runtime and control-plane comparison

Date: 2026-07-19

## Executive conclusion

OpenShip and Ployz overlap at the deploy button, but they are currently different
products underneath it:

- **OpenShip is a centrally coordinated application platform.** One API and one
  database own projects and deployment history. A self-hosted control plane
  operates a local Docker daemon or an SSH-reachable server; OpenShip Cloud uses
  the same product model over a cloud-runtime adapter.
- **Ployz is a small-cluster orchestration substrate.** One disposable core owns
  operator intent and operation evidence, while authenticated machine, gateway,
  and DNS roles own live testimony and runtime effects over NATS.

For a single VPS, OpenShip's model is much cheaper to build, install, explain, and
support. For a real multi-machine cluster with partial failure, authority,
placement, and recovery requirements, Ployz has the deeper model.

OpenShip has genuinely out-shipped Ployz in end-to-end product surface: a released
CLI, web dashboard, desktop app, REST API, MCP server, GitHub automation, preview
deployments, environment management, domains, backups, rollback UI, notifications,
and Compose editing. It has not yet out-shipped Ployz's multi-machine runtime. Its
own README lists multi-node clusters, private networking, load-balancing UI, and
advanced monitoring as future work.

## Evidence quality and claim drift

The public website is materially ahead of the repository and should be read as
positioning plus roadmap, not as the current implementation contract:

- The homepage says production machines never build and that built images stream
  to them. Current SSH Docker code transfers or clones source and invokes
  `docker build` on the target host. Local builds exist for some Bare/Cloud paths,
  but the Docker server path is a server build.
- The homepage advertises multi-server fan-out and private networking. The README
  says multi-node clusters and private networking are "coming next"; current
  self-hosted networking is one Docker bridge per project on one daemon.
- The homepage currently says AGPL-3.0; the repository and checked-in license are
  Apache-2.0.
- "No agent" is accurate in the narrow sense that no OpenShip machine agent runs
  on a target VPS. OpenShip does install and configure Docker, Git, OpenResty,
  certbot, system services, route files, and a remote operation journal there.

This comparison therefore uses the checked-in source at commit
[`97d917c4`](https://github.com/oblien/openship/tree/97d917c4f7b73985c2a6a28763c12e7f82a8c4db)
and its source-backed documentation, with the homepage used only for stated
positioning.

## OpenShip's actual model

### Control plane

The control plane is the OpenShip API running as a local service, desktop app,
self-hosted server, or SaaS. The dashboard talks only to that API. The same
codebase selects its role through configuration.

The API owns:

- organizations, permissions, projects, and configuration;
- deployment rows, active deployment pointers, domains, environment variables,
  build sessions, and logs;
- GitHub/webhook workflows, schedules, backups, notifications, and audit events;
- runtime, routing, TLS, and system adapters.

Self-hosted installs use embedded PGlite by default or PostgreSQL when configured.
The high-level deployment worker is in the API process. At boot, deployments left
in queued/building/deploying are marked cancelled; uncertain `reconciling`
deployments are handled by a periodic read-back task. Redis/BullMQ is optional for
some background jobs, with an in-process fallback.

Source:

- [Architecture overview](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/apps/web/content/docs/architecture/overview.mdx)
- [Database selection](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/packages/db/src/client.ts)
- [Boot sweeps and schedulers](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/apps/api/src/app.ts)
- [Deployment schema](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/packages/db/src/schema/deployment.ts)

### State ownership

OpenShip has a crisp single-owner rule at project granularity:

- local/server project: canonical only in the self-hosted database;
- cloud project: canonical only in the SaaS database, with no local shadow;
- promote/bring-home: copy the project, then remove it from the old owner.

A local instance proxies cloud-owned project calls to SaaS using the
organization owner's cloud session after enforcing local team permissions. This
avoids a local/cloud reconciliation protocol entirely. The cost is that Cloud is
the control-plane authority for every cloud-owned project, not merely a workflow
consumer.

Source:

- [Data ownership](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/apps/web/content/docs/architecture/data-ownership.mdx)
- [Cloud as source](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/apps/web/content/docs/architecture/cloud-as-source.mdx)
- [Project source router](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/apps/api/src/lib/cloud/project-router.ts)

### Runtime and target model

Each deployment freezes one target (`local`, `server`, or `cloud`) and one
runtime mode (`docker` or `bare` where self-hosted). The platform factory composes
four adapters: runtime, routing, SSL, and system setup.

For a remote server:

1. The control plane holds encrypted SSH credentials or uses the operator's SSH
   agent.
2. It installs/checks prerequisites and reaches Docker through an SSH tunnel.
3. The Docker path normally prepares source locally or clones on the target,
   transfers context when needed, and runs `docker build` on the target daemon.
4. It creates labeled containers, a per-project Docker bridge, and OpenResty
   route files; certbot handles certificates.
5. Docker restart policies or systemd/nohup keep workloads alive independently
   of the OpenShip API.

For Cloud, a `CloudRuntime` delegates workspace, build, routing, and lifecycle
operations to the managed compute API. The infrastructure below that API is not
described by the open-source control-plane repository.

Source:

- [Runtime model](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/apps/web/content/docs/architecture/runtime-model.mdx)
- [Platform composition](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/packages/adapters/src/platform.ts)
- [Docker SSH build/runtime](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/packages/adapters/src/runtime/docker.ts)
- [OpenResty system setup](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/packages/adapters/src/system/catalog.ts)

### Mutation and failure semantics

OpenShip is stronger here than "SSH scripts" implies. Mutating remote commands
can run through a per-server FIFO and a remote journal keyed by stable operation
id. If SSH drops, the control plane reconnects and harvests the recorded result
instead of blindly running the command twice. If no terminal journal result
exists, it reports an unknown/interrupted outcome rather than guessing.

At the product level, however, the unit of durability is still primarily a
deployment row plus build-session logs, not a typed append-only operation with a
closed transition algebra. Deployment status is a free-text database column.
High-level deploy work does not resume after an API restart. Failed deploy
cleanup normally removes new artifacts and containers, preserving logs but not
the complete failed scene.

When an SSH connection drops after containers may have started, OpenShip records
`reconciling`. A periodic task reads actual Docker/container state and settles it
to ready, partial failure, or failed. Bare mode cannot be inspected this way and
stays reconciling until superseded. This is honest uncertainty handling, but it
is a narrower mechanism than Ployz's machine testimony and operation evidence.

The remote server path also currently omits the post-activation readiness probe;
only a local target is probed before route switching. Thus a remote container can
be started and routed before application readiness is established.

Source:

- [Remote command journal](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/packages/adapters/src/system/remote-journal.ts)
- [SSH connection manager](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/apps/api/src/lib/ssh-manager.ts)
- [Deployment reconciliation](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/apps/api/src/modules/deployments/reconcile.service.ts)
- [Deploy sequencing](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/packages/adapters/src/runtime/deploy-pipeline.ts)
- [Target-specific readiness wiring](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/apps/api/src/modules/deployments/build-pipeline.ts)
- [Failure cleanup](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/apps/api/src/modules/deployments/deployment-lifecycle.ts)

### Control-plane outage

If the OpenShip API or database is unavailable:

- existing Docker/systemd processes and static OpenResty configuration continue
  serving;
- Docker restart policies still restart containers;
- deploys, configuration changes, dashboard/API access, live observation, and
  API-owned schedules stop;
- API restart cancels ordinary in-flight deploy tasks and later reconciles only
  the explicitly indeterminate cases.

A desktop-mode deploy writes a best-effort, structural, secret-free manifest to
the target server. Its source comment describes future/fresh-controller
re-adoption, but no generic project scan/adopt endpoint was found in current
main. It should not yet be treated as equivalent to Ployz core promotion.

Source: [server manifest](https://github.com/oblien/openship/blob/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/apps/api/src/lib/openship-manifest.ts).

## Side-by-side

| Dimension | OpenShip | Ployz |
| --- | --- | --- |
| Product category | Central application platform/control panel | Small-cluster orchestration core |
| Control authority | One API + PGlite/Postgres, or SaaS for cloud-owned projects | One disposable Core with core-local intent/evidence |
| Machine transport | Agentless SSH, Docker socket tunnel, shell/system effects | TLS-authenticated NATS services with machine-scoped credentials |
| Runtime owner | Central API drives Docker/Bare/Cloud adapters | Machine role owns Docker effects and machine-local fact ledger |
| Live truth | DB references plus direct Docker/Cloud inspection | Fresh machine/role testimony gathered from intent-known candidates |
| Mutation durability | Deployment row/logs; remote command journal for selected mutations; periodic reconcile for unknown outcomes | Typed operation admission, append-only events, bounded RPC, terminal typed result |
| Routing | OpenResty files per target host; cloud edge adapter | Independently supervised gateway role with last-known-good projected state |
| Service network | Per-project Docker bridge on one daemon | Native cross-machine endpoint network, placement, gateway, and DNS roles |
| Build | Generated/repo Dockerfile; target-host, local, or cloud depending on path | Exact-commit Dockerfile or Railpack Build Adapter, pinned toolchain, per-platform placement and OCI validation |
| Multi-architecture | Not a first-class self-hosted receipt/placement model found | First-class amd64/arm64 fan-out and content-addressed pushed-image receipt |
| Core/control loss | Apps/routes keep running; central canonical DB required to operate | Data plane keeps LKG; promote a machine using mirrored intent and fresh facts |
| Cloud relationship | SaaS is canonical owner and runtime authority for cloud projects | Cloud owns product workflow/history; cluster core remains runtime authority |
| Product surface | Broad, integrated, released in one monorepo | Deep core/SDK plus a separate, still-integration-heavy Cloud product |

Ployz sources:

- [Runtime vision](../../VISION.md)
- [Contributor code map](../architecture/code-map.md)
- [NATS control-plane architecture](../architecture/nats-control-plane.md)
- [Coreless v2 ADR](../adr/0040-corrosion-replaces-the-core-and-nats.md)
- [Build Adapter types](../../crates/ployz-core/src/build.rs)
- [Build placement and operation driver](../../crates/ployzd/src/control/operations/build/driver.rs)
- [Machine build executor](../../crates/ployzd/src/roles/machine/execution/build/runner.rs)
- [Pushed image receipt](../../crates/ployz-core/src/deploy/images.rs)

## Where OpenShip is stronger

1. **Time to first value.** `npm i -g openship`, `openship up`, and a dashboard is
   a much shorter path than installing and understanding a NATS-backed cluster.
2. **Vertical integration.** Git provider, build detection, deploy, domain, logs,
   metrics, rollback, backup, notification, CLI, GUI, API, and MCP are one product
   path rather than separate substrate and Cloud integration projects.
3. **Single-VPS economics.** No per-target product agent, no cluster membership,
   no mesh, no role credentials, and no promotion protocol are excellent trades
   when the actual problem is one Docker host.
4. **Project ownership clarity.** Fully local or fully cloud, never mirrored, is a
   clean way to prevent split-brain at the application-platform layer.
5. **Remote reliability seam.** Stable-id command journaling is a thoughtful
   answer to SSH disconnect ambiguity.
6. **Compose/product editing.** Its service-record import and three-way
   repo/dashboard merge is substantially beyond Ployz's deliberately narrower
   Compose input adapter.
7. **Adoption surface.** Apache-2.0 is simpler for downstream adopters than the
   Ployz license's hosted-competition restriction.

## Where Ployz is stronger

1. **Actual multi-machine semantics.** Intent-known membership, placement, typed
   silence, native networking, machine facts, and role health already exist as
   one model rather than a future SSH fan-out feature.
2. **Authority boundaries.** NATS identities and subject permissions give a
   machine only its scoped RPC authority. OpenShip's central control plane holds
   broad SSH/Docker authority over every registered server.
3. **Operation evidence.** Mutations have explicit typed transitions, durable
   progress, terminal finality, cleanup evidence, and typed failures. OpenShip's
   high-level status model is broader but looser.
4. **Failure inspection.** Ployz intentionally retains failed started containers
   and evidence. OpenShip generally cleans the new runtime scene on failure.
5. **Rollout correctness.** Ployz health-gates new service containers before
   promotion. OpenShip's current remote-server path does not run its readiness
   probe before routing.
6. **Build integrity.** Ployz freezes the exact commit, keeps Dockerfile and
   Railpack as closed adapters, pins and records the toolchain, validates OCI
   output, fans out by native architecture, and returns a typed content receipt.
7. **Control-plane recovery.** The disposable-core/promotion model is a designed
   cluster recovery path, not merely "workloads keep running while the DB is
   down."
8. **Truth semantics.** Ployz explicitly separates durable intent, live
   testimony, fanout invalidation, and operation evidence. OpenShip's DB and
   adapter layer carry more kinds of truth in one process.

## How OpenShip out-shipped Ployz

The main cause is scope, not superior distributed-systems architecture.

OpenShip chose one central owner, one database, privileged SSH, ordinary Docker,
and OpenResty. It accepted a large control-plane failure domain, target-host
builds, shell-based effects, free-text lifecycle state, and limited cluster
semantics. Those choices removed entire categories of work: membership,
machine-service authority, fresh testimony gathers, mesh convergence,
multi-platform placement, disposable-core promotion, and cross-role recovery.

It spent that saved complexity on the visible product loop. Ployz spent its
complexity budget on the runtime substrate and only later connected the Cloud
workflow. Consequently, OpenShip can show a broad, coherent application platform
today while Ployz has a more defensible engine whose hosted happy path is not yet
equally complete.

There is also an execution lesson: OpenShip has tolerated seams that Ployz would
normally refuse to ship, and its marketing is ahead of those seams. Ployz should
not copy the claim drift, but it should copy the insistence on a complete golden
path before broadening the platform.

## Implication for hosted builders and Railpack

OpenShip validates separating **where a build runs** from **where the workload
runs**, but its `RuntimeAdapter` is not the interface Ployz should copy.

Ployz already has the better boundary:

- `BuildAdapter::{Dockerfile, Railpack}` says how source becomes an image;
- the build operation driver selects an execution venue;
- the result is a platform-indexed, content-addressed receipt consumed by deploy.

The elegant hosted-runner change is therefore a new **build execution venue** (or
executor authority), not a new Build Adapter and not a fake cluster Machine:

```text
BuildAdapter: Dockerfile | Railpack         unchanged
BuildVenue:   ClusterMachine | HostedRunner new placement choice
Output:       validated OCI platforms
Handoff:      push to an authorized cluster seed or registry
Result:       the existing deploy-consumable receipt
```

Railpack remains exactly as it is. A hosted runner receives a frozen exact-commit
job and short-lived source credential, uses the same pinned Railpack/BuildKit
toolchain, emits the same toolchain/log/failure evidence, and gets only
request-scoped authority to place output. It should either push the validated OCI
content to a chosen cluster seed and return the existing `PushedImageReceipt`, or
publish to a registry and return a registry-backed artifact contract. It should
not become cluster membership and should never receive general machine-control
credentials.

That gives Ployz OpenShip's "production machines do not build" benefit without
giving SaaS ownership of cluster truth or sacrificing Railpack.

## Primary OpenShip sources

- [User-supplied First Deployment guide](https://openship.io/docs/first-deployment)
- [OpenShip repository and current README](https://github.com/oblien/openship)
- [Public homepage and positioning](https://openship.io/)
- [Checked-in architecture docs](https://github.com/oblien/openship/tree/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/apps/web/content/docs/architecture)
- [Checked-in runtime adapters](https://github.com/oblien/openship/tree/97d917c4f7b73985c2a6a28763c12e7f82a8c4db/packages/adapters/src/runtime)
