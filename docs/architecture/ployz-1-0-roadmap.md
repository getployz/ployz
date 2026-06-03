---
title: Ployz 1.0 Roadmap
status: draft
created: 2026-05-24
origin:
  - VISION.md
  - docs/plans/2026-05-24-002-feat-ployz-1-0-cli-shape-and-workflows-plan.md
  - docs/plans/2026-05-24-003-feat-ployz-1-0-state-and-substrate-plan.md
  - docs/plans/2026-05-24-004-feat-ployz-1-0-deploy-branch-volume-plan.md
---

# Ployz 1.0 Roadmap

## 1.0 Definition

Ployz 1.0 is the minimal runtime orchestrator behind the launch promise:
Ployz Cloud is Railway, but it runs on your own servers. The core provides a
Docker/Compose-style runtime contract across 1-200 owned servers, with
explicit deploys, overlay networking, HTTP/HTTPS routing, ACME-managed
certificates, and Ployz-managed ZFS volumes from day one.

Cloud owns the rich product: canvas, environments, GitHub, builders, AI,
billing, deploy queues, and workflow history. Inngest owns multi-step product
processes. Rust owns small runtime operations and the facts needed to prove
what is running on customer-owned machines.

The 1.0 product must prove the permanent substrate:

- machines join, stay identifiable across restart, and expose clear
  diagnostics;
- namespace is a typed grouping label on containers, volumes, and
  certificates, not a durable table;
- services deploy through previewable plans that create, replace, and remove
  Ployz-owned containers;
- service identity is derived from container rows, not a durable services
  table;
- HTTP/HTTPS routes and gateway config are derived from container specs,
  container observation, and usable certificate state;
- persistent state uses Ployz-managed ZFS datasets, not opaque Docker volumes
  or host bind mounts;
- stateful service placement respects volume ownership;
- cloud and agents can drive the same public CLI/API without private
  orchestration semantics.

The 1.0 core does not ship branch environments, namespace lifecycle,
promotion, rollback, volume clone/fork, cross-machine volume movement, or
machine drain/remove as core-owned primitives. Cloud may model product
environments and workflow history now; the core keeps only the runtime hooks
those future primitives need: typed namespace labels embedded in Ployz-owned
resource rows, deploy specs embedded in Ployz-owned container rows, stable
service identity labels, volume rows, ZFS dataset identity, volume owner
machines, certificate state, ACME attempt evidence, and gateway projection
from container/certificate truth.

The product should stay closer to `~/dev/uncloud` simplicity than the old
legacy codebase:

- direct commands;
- typed product specs;
- explicit plan/confirm/execute flows;
- small operation structs;
- a small durable row set;
- thin Corrosion access;
- services, routes, and deploys as derived command surfaces;
- no hidden desired-state controller;
- no generic substrate framework invented before it is needed.

## Non-Negotiable Architecture

- `polis` owns substrate primitives: Corrosion rows, transactions,
  subscriptions, iroh identity, tickets, peer RPC, probes, deadlines, and
  distributed failure typing.
- `ployz` owns product behavior: machine lifecycle, namespace labeling, deploy
  semantics, container runtime identity, routing projection, ACME certificate
  lifecycle, ZFS-backed volume ownership, readiness, placement, and operation
  outcomes.
- Ployz adapters translate between product ports and Polis primitives.
- Corrosion stores row-shaped cluster state. It is not the command bus.
- iroh RPC carries bounded peer commands.
- WireGuard peers are the machines in the same authority island for 1.0.
  Dynamic namespace-scoped networking is a post-1.0 optimization to reduce mesh
  scope for large islands.
- Post-1.0, separate authority islands communicate by RPC. A laptop can be its
  own island and ask a production island to deploy resources; the production
  island authorizes and writes its own rows.
- Tickets are bootstrap envelopes. Durable identity is iroh endpoint ID.
- Owner-machine serialization is the default fence for stateful resources.
  Coordinators RPC to the resource owner, and that owner enforces local
  ordering before writing its Corrosion rows. Explicit distributed claims are a
  later escape hatch for a proven multi-owner path.
- Inngest is the workflow engine for the cloud product. Core operations are
  bounded runtime transitions with typed results, not durable product
  workflows.
- Cloud Postgres may store product state, editor state, deploy workflow state,
  and cached observations. It must not become the source of cluster runtime
  truth.

## Roadmap Tracks

### Track A: CLI And Public Contract

Goal: make the small runtime contract concrete before implementation spreads,
so cloud, Inngest, CLI, and agents can all call the same operations.

Deliverables:

- root CLI crate/binary;
- global connection/context handling;
- command tree from the CLI plan;
- human and JSON output envelopes;
- preview/apply/verify conventions;
- exit-code contract;
- operation result schemas for cloud workflow persistence;
- public API structs shared by CLI/cloud/agents.

First slices:

1. `ployz status` and `ployz doctor` over local daemon/substrate diagnostics.
2. `ployz machine list/inspect`.
3. `ployz deploy preview` rendering a plan from an in-memory fixture.
4. `ployz deploy apply --yes` calling the product engine.

Done when:

- the cut 1.0 workflows have visible commands: status, doctor, machine
  list/inspect/add, namespace list/inspect, deploy preview/apply/verify,
  service list/inspect/logs, container list/inspect/logs, and volume
  list/inspect/create;
- namespace list/inspect are derived from containers, volumes, and
  certificates; there is no namespace create command in 1.0;
- container, route, and certificate status is visible from service inspection
  and doctor output;
- JSON output can be consumed by a zero-context agent without parsing human
  text;
- command help text names the risk and confirmation behavior for mutating
  commands.

### Track B: Substrate Spine

Goal: two daemons can discover each other, run RPC, sync Corrosion rows, and
survive restart with stable identity.

Deliverables:

- local iroh key load/create;
- endpoint and RPC server lifecycle;
- bootstrap ticket create/join;
- Corrosion process lifecycle;
- schema apply;
- store transaction/query/subscription primitives;
- machine row upsert/observe;
- two-node membership e2e.

First slices:

1. Finish Corrosion store primitive over `corro-client`.
2. Add iroh endpoint/RPC smoke test without Corrosion.
3. Add machine membership vertical slice.
4. Add restart identity/adoption test.

Done when:

- a returning machine keeps the same endpoint ID;
- `machine add` writes/observes rows through Corrosion;
- peer RPC has explicit deadlines and typed failures.

### Track C: Authority Island Mesh

Goal: network every machine in the authority island together. Namespaces do
not change network policy in 1.0.

Deliverables:

- authority island peer query;
- local WireGuard controller/adoption;
- mesh diagnostics.

First slices:

1. Derive full authority island peer set for one machine.
2. Rebuild WireGuard config from derived peers.
3. Expose `doctor mesh`.

Done when:

- machines in the same authority island get network edges;
- a daemon restart rebuilds the same mesh without rewriting cluster truth.

### Track D: Runtime Backend

Goal: deploy operations can start, verify, stop, inspect, and clean up
workloads on target machines.

Deliverables:

- internal runtime RPC protocol;
- runtime backend contract;
- local container backend;
- health/readiness checks;
- logs/exec basics;
- adoption of already-running instances.

First slices:

1. Start/stop/inspect a trivial workload on one machine.
2. Add readiness check with timeout.
3. Add logs surface for failed container starts.
4. Add restart adoption.

Done when:

- runtime peer commands do not deserialize public CLI requests over peer RPC;
- runtime errors are typed by caller action: retry, repair, unsupported, or
  peer unavailable.

### Track E: Deploy MVP

Goal: deploy Docker/Compose-style HTTP/HTTPS services under one namespace
label by creating Ployz-owned containers and deriving service/route/namespace
views from them.

Deliverables:

- deploy manifest/spec model for image-backed services;
- planning state over machines, volumes, current containers, and certificate
  status;
- typed deploy operations;
- image availability/distribution primitive;
- durable container rows carrying namespace, service identity, runtime
  identity, machine, image/spec digest, ports, volume refs, and restart
  adoption data;
- local container spec persistence so a machine can rehydrate Ployz-owned
  containers after daemon restart;
- certificate material/status rows and ACME account, order, challenge, and
  attempt rows;
- HTTP-01 challenge routing;
- gateway/DNS/certificate projection from container and certificate rows;
- basic service/container logs and status surfaces.

First slices:

1. Manifest validation/defaults for image, command, env, ports, healthcheck,
   replica count, route/hostname, ACME policy, and volume references.
2. Pure planning tests from current container rows for one service, then
   multiple services under one namespace label.
3. Runtime apply on one machine, including durable container row write and
   restart adoption.
4. ACME challenge, certificate readiness, and HTTPS projection.
5. `deploy verify`, service logs, and container inspection.

Done when:

- `ployz deploy preview/apply/verify` works for image-backed services;
- service, route, and namespace views can be reconstructed from container,
  volume, and certificate rows without durable service, route, namespace, or
  deploy tables;
- failed readiness or failed certificate issuance does not promote an HTTPS
  route;
- failed candidate containers remain inspectable when they were created.

### Track F: ZFS Volumes

Goal: make stateful operations a first-class product primitive.

Deliverables:

- volume rows;
- ZFS backend contract;
- fresh volume create;
- dataset identity and mountpoint adoption;
- owner machine, scope, and quota;
- container rows/specs reference attached volume ids;
- deploy planning that pins stateful services to volume owners;
- rejection for unsafe multi-writer and multi-replica stateful shapes;
- pool and volume doctor output.

First slices:

1. ZFS create/adopt/destroy local test.
2. `volume create`.
3. Deploy with a fresh managed volume.
4. Stateful placement pinned to the owner machine.
5. Pool, dataset, mount, quota, and owner diagnostics.

Done when:

- persistent service data is always under a Ployz-managed ZFS dataset;
- stateful services do not start on machines that do not own their volumes;
- volume usage can be derived from Ployz-owned container rows without a
  separate attachment table in 1.0;
- volume rows contain enough durable identity to add clone, rollback, and move
  later without migrating user data.

## Execution Order

### Milestone 0: Keep Current Corrosion/Iroh Slice Honest

- Completed by the substrate-spine e2e slice: real Corrosion lifecycle/schema,
  real iroh peer preflight, Corrosion-backed machine add, two-node row
  visibility, and restart-stable endpoint identity.
- Old p2panda/NATS/fact-store guidance is historical when it conflicts with
  the current Corrosion + iroh substrate direction.
- Machine row comments document row ownership and why the current machine
  `epoch` is only an owner-issued row version, not a global conflict solution.

### Milestone 1: CLI Skeleton And Substrate Smoke

- Add CLI crate and root command.
- Add `status`, `doctor`, `machine list`, `machine inspect`.
- Add local daemon substrate startup: `ployzd` composes the Polis Corrosion
  agent, applies membership schema, starts persistent iroh identity, reports
  typed substrate startup state, and shuts down cleanly.
- Reuse the substrate-spine e2e as the daemon startup regression target.

### Milestone 2: Authority Island Mesh

- Derive full authority island WireGuard peers.
- Add mesh diagnostics.

### Milestone 3: Single-Service Deploy

- Add manifest model.
- Add pure planner.
- Add runtime RPC start/verify/stop.
- Add durable container rows and local container spec persistence.
- Add certificate status/attempt rows.
- Ship one-service `deploy preview/apply/verify`.

### Milestone 4: Compose-Style Deploy

- Extend the manifest/spec to multiple image-backed services.
- Add env, command, ports, healthcheck, replica count, and HTTP/HTTPS route
  support.
- Add ACME HTTP-01 challenge handling, certificate issuance, activation, and
  renewal status.
- Add service/container logs and status surfaces.
- Derive service, route, and namespace views from container/certificate rows.
- Keep HTTPS projection gated by container readiness and certificate usability.

### Milestone 5: ZFS Volumes

- Add ZFS backend.
- Add volume rows, owner machine, dataset identity, scope, and quota.
- Add fresh volume create and deploy with volume attach.
- Pin stateful placement to volume owners.
- Add volume and pool doctor output.

### Milestone 6: Hardening For 1.0

- Full e2e matrix across two or more nodes.
- Crash/restart tests during every deploy checkpoint class.
- Corrosion subscription resume tests.
- RPC deadline/failure tests.
- ACME HTTP-01, renewal, and certificate failure visibility tests.
- ZFS create/adopt/mount/quota/doctor tests.
- CLI JSON compatibility tests.
- Docs: operator guide, failure guide, volume guide, architecture guide.

## Post-1.0 Feature List

The following Rust/core primitives are explicitly outside the 1.0 release.
They should build on the 1.0 substrate instead of changing it. Cloud may still
ship product UX around these areas when it can do so through existing bounded
core operations and Inngest workflow state.

- rolling deploy strategies beyond the minimum readiness-gated route switch;
- durable deploy/operation history beyond current command output and container
  evidence;
- durable namespace lifecycle: create, delete, metadata, ownership, tombstones,
  empty namespaces, and namespace-level policy;
- branch/PR environments;
- per-resource source policy and namespace lineage UX;
- volume snapshot UI, fork/clone, and clone-backed branch data;
- cross-machine volume move with ZFS send/receive;
- machine drain and safe machine remove;
- promote from a prepared namespace to production;
- rollback compiler, including stateful rollback once snapshot evidence is
  strong enough;
- Compose import refinements beyond the 1.0 spec subset;
- wildcard certificates, DNS-01 automation, custom certificate upload, and
  non-ACME certificate providers;
- autoscaling;
- provider-native database branches;
- multi-source branch composition;
- in-core AI/operator workflow execution.

## Simplicity Checks Before Each Slice

Ask these before implementing:

- Can this be a typed operation plus direct apply, like uncloud, rather than a
  controller?
- Is this a durable fact, or can it be derived from machines, containers,
  volumes, and certificates?
- Does this row have one obvious owner?
- Is JSON only used for opaque metadata, not a set/map with independent
  writers?
- Is this a Ployz product concept that should stay out of Polis?
- Is this a cloud product workflow that belongs in Inngest instead of Rust?
- Can the preview prove the dangerous part before mutation?
- Does failure name the audience and next action?
- Can a daemon restart adopt the last good state?
- Can the next slice be useful without finishing all future slices?

## 1.0 Release Gates

- CLI workflows documented in the CLI plan work in e2e tests.
- Two-node cluster passes public machine add, deploy, HTTP/HTTPS routing,
  ACME-backed certificate issuance, service/container logs/status, derived
  namespace list/inspect, and ZFS-backed volume create/attach tests.
- Services, routes, and namespaces are reconstructable from Ployz-owned
  container rows, volume rows, and certificate rows; 1.0 has no durable
  services, routes, namespaces, deploy records, or deploy phase tables.
- Failed certificate issuance or renewal is visible in CLI/API output and does
  not silently publish an unusable HTTPS route.
- A stateful service cannot be deployed outside the machine that owns its
  attached single-writer volume.
- Every external control-plane I/O path has a deadline.
- Every mutating command has human and JSON output.
- Corrosion schema changes are additive and file-backed.
- No ordinary Ployz module imports Corrosion, iroh, or irpc types.
- No hidden background task rewrites product truth.
- Failed-before-commit and failed-after-checkpoint states are visible and
  recoverable.
