# Ployz Cloud — Dashboard Design

The paid layer is a Railway-style dashboard over the OSS daemon. The daemon remains
the source of truth for all operational state. The dashboard is a lens, not a brain.
Users can adopt (connect clusters) and eject (disconnect, keep everything running)
cleanly.

## Architecture

```
CLI ──────────► Daemon API ◄─────────── Dashboard
                    │                        │
                Corrosion                 Cloud DB
              (source of truth)        (read cache +
               survives eject)       user/billing/teams)
```

The dashboard calls the same `DaemonRequest` operations as the CLI. A per-cluster
agent (paid component) relays status from Corrosion to the cloud DB and forwards
deploy commands from the dashboard to the daemon.

### What the cloud DB owns (lost on eject, acceptable)

- Users, teams, API keys, billing
- Cluster registration (name, callback URL, join tokens)
- Canvas objects and edges (the authoring model)
- Deploy queue and audit log
- Alerting rules, notification preferences

### What the cloud DB caches (survives eject in Corrosion)

- Machine records, instance status, service heads, deploy records
- Polled/streamed from agent for dashboard rendering
- Stale cache is fine — worst case the UI is behind by seconds

### What the cloud DB does NOT store

- `ServiceSpec` definitions that the daemon reads back — those live in Corrosion
- Instance assignments / slot placement — daemon decides
- Any state the daemon depends on to function

## Concept mapping

| Railway    | Ployz                                              |
|------------|----------------------------------------------------|
| Org        | Cluster (set of machines in one mesh)              |
| Project    | Namespace                                          |
| Service    | ServiceSpec (container + routing + placement)      |
| Deploy     | DeployRecord (with DeployPreview and events)       |
| Instance   | InstanceStatusRecord (per-slot container)           |

## Canvas model

The canvas is richer than a raw `DeployManifest`. It has higher-level objects that
compile down to `ServiceSpec`s at deploy time.

### Canvas objects

```
CanvasObject
  ├── Service        → produces a ServiceSpec
  ├── SharedEnv      → merges into connected services' template.env
  ├── Autoscaler     → controls connected service's placement.count
  ├── Database       → (future) provisions + injects connection string
  ├── Volume         → shared volume referenced by multiple services
  └── ...extensible
```

### Canvas DB schema

```
canvas_objects
  ├── id (uuid)
  ├── cluster_id
  ├── namespace
  ├── kind (service | shared_env | autoscaler | ...)
  ├── config_json (kind-specific payload)
  ├── position_x, position_y (canvas layout)
  └── updated_at, updated_by

canvas_edges
  ├── from_id → canvas_object.id
  ├── to_id   → canvas_object.id
  ├── edge_kind (uses_env | scales | attaches_volume | ...)
  └── metadata_json
```

### Compilation

Canvas objects + edges compile to a flat `DeployManifest`:

1. Start with each service's own config
2. Walk inbound edges, merge shared envs (conflict = error)
3. Resolve autoscaler → set `placement` count
4. Emit `ServiceSpec` per service
5. Wrap in `DeployManifest { services: [...] }`

The compiled output is identical to what `ployz deploy -f manifest.json` accepts.
No translation layer. Eject = export canvas as manifest JSON, use CLI going forward.

### Autoscaler

Runs as a control loop in the paid agent, outside the deploy cycle:

```
loop {
    current = read placement from cluster
    desired = evaluate(metrics)
    if desired != current:
        update canvas object → recompile → trigger deploy
}
```

The daemon only ever sees `Replicated { count: N }`. On eject, the user keeps
whatever count was last deployed.

## Diffing

### Two-tier preview

| What                       | Where               | When                    |
|----------------------------|---------------------|-------------------------|
| Field-level config diff    | wasm, client-side   | every edit (debounced)  |
| Slot/machine deploy plan   | daemon `preview()`  | on "Review deploy" click|

### Field-level diffing

Both specs are `serde_json::Value`. Use `sjdiff` (pure Rust, wasm-compatible) to
diff at the JSON layer. No new derives needed on SDK types. Each atomic change
produces one line:

| User action       | Diff line                                           |
|-------------------|-----------------------------------------------------|
| Change image      | `image: app:v1 → app:v2`                           |
| Add env var       | `env.NEW_KEY: + "value"`                            |
| Remove env var    | `env.OLD_KEY: - "value"`                            |
| Edit env var      | `env.PORT: "3000" → "8080"`                         |
| Change replicas   | `placement: singleton → replicated(3)`              |
| Add route         | `routes[1]: + http(api.example.com)`                |

### Attribution

Since canvas objects compile to specs, diff lines can carry attribution:
"env.DB_HOST added (from shared-env 'db-creds')". The compilation step tracks
which canvas object contributed each field.

### Wasm crate

A small `ployz-diff` wasm crate (or in the dashboard repo) exposes:

```rust
fn compile(objects: &[CanvasObject], edges: &[CanvasEdge]) -> DeployManifest
fn diff_compiled(before: &str, after: &str, attribution: &AttributionMap) -> Vec<SpecChange>
```

### OSS additions that benefit both CLI and dashboard

1. **`FieldChange` in `ServicePlan`** — field-level diff in `preview()` output
2. **`SlotDowntime` in `SlotPlan`** — estimated migration cost per slot
3. **Populate `warnings` vec** — excluded machines, image pull predictions
4. **Streaming `apply()`** — emit events via channel for real-time progress

## Deploy staging — the three-state model

### Problem

Naive diffing (canvas vs actual) flickers during deploys. Changes appear staged
until the cluster catches up, then vanish one-by-one as instances report ready.

### Solution: three states

```
actual_state     ← agent syncs from cluster (eventually consistent)
deploy_target    ← frozen manifest snapshot from when user clicked deploy
canvas_desired   ← live canvas (user can keep editing)
```

### The baseline rule

```
if deploy_queue has any active (queued or in-flight) deploys:
    effective_base = last active deploy's manifest
else:
    effective_base = actual_state

staged_diff = diff(canvas_desired, effective_base)
```

Clicking deploy shifts the baseline to the frozen manifest. Staged changes vanish
instantly — no waiting for cluster convergence. New edits after the click appear
as new staged changes against the deploy target.

### Deploy lifecycle

```
User edits canvas         → staged_diff updates (canvas vs actual)
User clicks deploy        → manifest frozen as D1
                            staged_diff = canvas vs D1.manifest = empty
User edits more           → staged_diff = canvas vs D1.manifest = new edits only
User clicks deploy again  → manifest frozen as D2 (queued behind D1)
                            staged_diff = canvas vs D2.manifest = empty

D1 succeeds               → actual_state catches up, D2 starts
D1 fails                  → cancel queued deploys
                            effective_base falls back to actual_state
                            all undeployed changes resurface as staged
```

### Deploy queue data model

```typescript
interface Deploy {
  id: string
  namespace: string
  manifest: DeployManifest          // frozen at submit time
  status:
    | { phase: "queued" }
    | { phase: "in_flight", events: DeployEvent[] }
    | { phase: "committed", deploy_id: string }
    | { phase: "failed", error: string, events: DeployEvent[] }
    | { phase: "cancelled", reason: string }
  submitted_at: number
  submitted_by: string
}
```

### Failure behavior

The daemon's deploy is atomic at the commit level — all service heads commit in
one Corrosion transaction or none. On failure:

1. Mark deploy as failed with error and events
2. Cancel all queued deploys
3. Baseline falls back to `actual_state`
4. All uncommitted changes resurface as staged
5. User sees failure details, can fix and re-deploy

### Canvas rendering during deploy

```
┌─────────────────────────────────────────────────────────┐
│  Canvas                                                  │
│                                                          │
│  ┌─ api ──────────────┐  ┌─ worker ───────────────────┐ │
│  │ (no staged changes)│  │ env.LOG: added (staged)     │ │
│  └────────────────────┘  └─────────────────────────────┘ │
│                                                          │
│  ┌─ Deploy D1 (in progress) ──────────────────────────┐  │
│  │ ✓ api: image app:v1 → app:v2        committed      │  │
│  │ ● redis: starting candidate on machine-a            │  │
│  │ ○ worker: waiting                                   │  │
│  └────────────────────────────────────────────────────┘  │
│                                                          │
│  1 staged change · 1 deploy in progress                  │
│  [Deploy ▶]                                              │
└─────────────────────────────────────────────────────────┘
```

## Canvas node catalog

Every node follows the extraction pattern: start inline on a service, extract to
a shared node, optionally swap for an external provider. And every node that
provisions infrastructure supports per-environment providers — container in dev,
external in prod.

### Node types

#### Compute nodes (produce running containers)

| Node | Compiles to | Notes |
|------|------------|-------|
| **Service** | `ServiceSpec` | The primary unit. Has ports, routes, env, volumes |
| **Worker** | `ServiceSpec` (no ports/routes) | Background processor, visually distinct on canvas |
| **CronJob** | `ServiceSpec` + agent schedule | Agent starts/stops on cron. In PR envs: disabled or run-once |
| **Migration** | Runs-to-completion container | Runs AFTER database ready, BEFORE services start |
| **InitTask** | Runs-to-completion container | Runs before its dependent service. Edge: `runs_before` |
| **TestSuite** | Runs-to-completion container | Runs AFTER services ready. Reports to GitHub checks |
| **Seed** | Runs-to-completion container | Data seeding, alternative to ZFS snapshot clone |

#### Data nodes (produce volumes + connection vars)

| Node | Outputs | PR strategy |
|------|---------|-------------|
| **Database** | `DATABASE_URL`, `DB_HOST`, etc. | ZFS snapshot clone / branch (PlanetScale, Neon) / seed script / empty+migrate |
| **Cache** | `REDIS_URL`, `CACHE_URL` | Fresh container (ephemeral, no clone needed) |
| **Queue** | `AMQP_URL`, `NATS_URL` | Fresh container |
| **ObjectStore** | `S3_ENDPOINT`, `S3_BUCKET`, credentials | ZFS snapshot clone / fresh MinIO |
| **Volume** | `VolumeSource::Managed` on connected services | ZFS snapshot clone / fresh |
| **SearchIndex** | `SEARCH_URL` | Fresh container (MeiliSearch, Typesense) / external (Algolia) |

#### Config nodes (produce env vars / files)

| Node | What it does | Lifecycle hooks |
|------|-------------|-----------------|
| **InlineEnv** | Key-values on the service itself | — |
| **SharedEnv** | Extracted key-values, connectable to N services | — |
| **ExternalEnv** | Fetched from provider at deploy time | `on_deploy`: fetch latest |
| **Secret** | Encrypted value, audit-logged access | `on_deploy`: fetch + rotate |
| **ConfigFile** | File content mounted into containers | — |
| **.env File** | Uploaded/pasted, stored encrypted in cloud DB | — |

External env providers: 1Password, Hashicorp Vault, Doppler, AWS SSM,
Infisical, Azure Key Vault, GCP Secret Manager.

All config nodes produce the same thing: key-value pairs merged into
`template.env` (or files mounted into `template.volumes`). The daemon never
knows the source.

#### Networking nodes

| Node | Compiles to |
|------|------------|
| **Domain** | `RouteSpec::Http { hostnames }` on target service |
| **TCPEndpoint** | `RouteSpec::Tcp { listen_port }` |
| **Certificate** | Custom TLS cert pushed to gateway |
| **NetworkPolicy** | (future) Firewall rules between services on overlay |
| **Tunnel** | Agent-managed tunnel for external access (agent/CI use) |

#### Operations nodes

| Node | Lives where | What it does |
|------|-----------|-------------|
| **Autoscaler** | Agent control loop | Adjusts `placement.count` from metrics |
| **ResourceProfile** | Canvas node | Shared `Resources { cpu, memory }` applied to connected services |
| **HealthCheck** | Canvas node | Shared `ReadinessProbe` applied to connected services |
| **AlertRule** | Cloud DB | Fires notification on condition |
| **LogDrain** | Agent sidecar | Ships container logs to Datadog/Grafana/S3 |

#### Orchestration nodes (PR/environment lifecycle)

| Node | Purpose |
|------|---------|
| **Snapshot** | ZFS snapshot source for cloning data volumes |
| **Notification** | Posts to GitHub PR, Slack, webhook on lifecycle events |

#### Reference nodes (point outside the canvas)

| Node | Purpose |
|------|---------|
| **ExternalService** | Service in another namespace (e.g., shared auth, staging backend) |
| **ExternalDatabase** | Managed DB with no container (just connection vars) |
| **ExternalAPI** | Third-party API, just env vars and optional health check |

### Edge types

| Edge | From → To | Semantics |
|------|----------|-----------|
| `injects_env` | config/data node → compute node | Merge key-values into `template.env` |
| `mounts_volume` | volume/data node → compute node | Add to `template.volumes` |
| `connects_to` | data node → compute node | Provisions + injects connection env |
| `routes_to` | domain → compute node | Adds to `routes` / `hostnames` |
| `scales` | autoscaler → compute node | Controls `placement` |
| `runs_before` | init task/migration → compute/data node | Startup ordering |
| `tests` | test suite → compute node | Runs after ready, reports results |
| `applies_profile` | resource profile → compute node | Sets `template.resources` |
| `applies_health` | health check → compute node | Sets `readiness` |
| `drains_to` | compute node → log drain | Ships logs |
| `alerts_on` | compute node → alert rule | Monitors |
| `snapshots_from` | PR data node → source data node | ZFS clone source |
| `notifies` | environment → notification | Lifecycle events |
| `exposes_via` | compute node → tunnel | External access |

Multiple config nodes can connect to a single service. All inbound `injects_env`
edges merge. Conflicts (same key from two sources) are an error unless the user
sets priority on the edge.

### Provider-per-environment model

Canvas objects have providers that vary by environment. The object interface
(output vars, edges) stays the same. Only the backend changes.

```
Database "main-db"
  interface: postgres
  outputs: [DATABASE_URL, DB_HOST, DB_PORT, DB_USER, DB_PASSWORD]

  providers:
    local:       { kind: container, image: postgres:16 }
    staging:     { kind: container, image: postgres:16, resources: large }
    pr-*:        { kind: snapshot_clone, from: staging }
    production:  { kind: external, static_vars: { DATABASE_URL: "postgres://prod..." } }
```

Provider resolution walks the environment hierarchy:

```
base (canvas template)
  └── local (all containers)
       └── staging (containers, maybe larger resources)
            ├── pr-* (snapshot_clone from staging, ephemeral)
            └── production (mostly external providers)
```

If an object has no explicit provider for an environment, it inherits from the
parent environment.

Compilation per environment:

```
compile(canvas, env="local"):
  Database "main-db" → provider=container
    → emits ServiceSpec for postgres:16
    → emits DATABASE_URL=postgres://main-db:5432/app

compile(canvas, env="production"):
  Database "main-db" → provider=external
    → emits NO ServiceSpec (nothing to deploy)
    → emits DATABASE_URL=postgres://prod.planetscale.io/...
```

Connected services are identical across environments. Only the data node
resolution changes.

### Lifecycle hooks

Any canvas object can have hooks that run during environment lifecycle events.
Hooks run in the agent, not in the cluster. They output `KEY=VALUE` lines on
stdout which become dynamic env vars.

```
LifecycleHook:
  trigger: on_create | on_destroy | on_deploy | on_schedule
  runtime: bash | deno | docker
  script: string
  timeout_seconds: number
  outputs: env (KEY=VALUE lines on stdout)
```

Hook outputs are stored per-environment-instance in the cloud DB. Static vars
(always present) + dynamic vars (from hooks) both merge into connected services.

Example — PlanetScale database node:

```
Database "main-db":
  static_vars:
    PLANETSCALE_ORG: myorg
    PLANETSCALE_DB: myapp
  hooks:
    on_create:  pscale branch create myapp $ENV_NAME --from main
                → outputs DATABASE_URL
    on_destroy: pscale branch delete myapp $ENV_NAME
  providers:
    local:      { kind: container, image: postgres:16 }
    staging:    { kind: container, image: postgres:16 }
    pr-*:       { kind: external, hooks: [on_create, on_destroy] }
    production: { kind: external, static_vars: { DATABASE_URL: "..." } }
```

In local/staging: runs a postgres container, no hooks.
In PR environments: hooks create a PlanetScale branch, output the URL.
In production: static connection string, no hooks.

## PR environments

### The ZFS advantage

The cluster runs on ZFS. Every data node's volume is a ZFS dataset. Creating a
PR environment clones staging data in milliseconds via `zfs clone`, regardless
of data size. The clone is copy-on-write — only delta writes consume space.

### Atomic multi-volume snapshots

`zfs snapshot -r data/staging` snapshots ALL child datasets atomically —
postgres, minio, elasticsearch, everything. The PR environment gets a consistent
point-in-time view across all data stores.

### PR creation DAG

Environment creation is a dependency graph. The agent resolves it and runs
phases with maximum parallelism.

```
Phase 1: Infrastructure (all parallel)
  ├── zfs clone staging-postgres → pr-postgres          ~0.1s
  ├── zfs clone staging-minio → pr-minio                ~0.1s
  ├── Start redis container (fresh, no data to clone)   ~2s
  ├── Start rabbitmq container (fresh)                  ~2s
  ├── Fetch secrets from 1Password (on_create hook)     ~1s
  └── Generate PR subdomain (wildcard DNS, instant)     ~0s

Phase 2: Data services (after clones ready)
  ├── Start postgres on cloned volume                   ~3s
  └── Start minio on cloned volume                      ~2s

Phase 3: Data transforms (after data services ready)
  ├── Run migrations on postgres (delta only)           ~2s
  └── Run data masking hook (PII → fake data)           ~3s

Phase 4: Application services (after all env resolved)
  ├── Start api (image cached on same machine)          ~5s
  ├── Start worker                                      ~5s
  └── Start frontend                                    ~5s

Phase 5: Validation (after services ready)
  ├── Run smoke test suite                              ~3s
  └── Post GitHub PR comment with URLs                  ~0s

Critical path: clone(0.1s) → postgres(3s) → migrate(2s) → api+readiness(5s) → smoke(3s)
Total: ~13 seconds for a full environment with realistic data
```

### PR update (new commits pushed)

```
1. Rebuild changed images (or pull new tags)
2. Recompile canvas (hooks don't re-run, dynamic vars still valid)
3. Diff against running state
4. Deploy delta only (probably just image updates)
```

Hooks with `on_deploy` trigger DO re-run (e.g., refresh secrets). Hooks with
`on_create` trigger do NOT re-run.

### PR close

```
1. Run on_destroy hooks (delete PlanetScale branch, etc.)
2. Tear down namespace (remove all services)
3. zfs destroy cloned datasets (instant space reclaim)
4. Delete ephemeral canvas instance
```

### Data strategies per node type

Each data node can use a different strategy for PR environments:

| Strategy | Best for | Speed |
|----------|---------|-------|
| `snapshot_clone` | Large databases, file storage. ZFS clone from staging | ~0.1s |
| `branch` | PlanetScale, Neon (provider-native branching) | ~5-15s |
| `seed` | Small datasets, custom test data. Runs a seed script | varies |
| `empty` | No data needed. Just run migrations on empty DB | ~2s |
| `fresh` | Caches, queues. No state to preserve | ~2s |
| `shared` | Feature flags, auth service. Reuse staging instance | 0s |

### Data masking

A hook on database nodes that runs AFTER clone, BEFORE services start:

```
Database "main-db":
  hooks:
    on_create:
      1. zfs clone (handled by provider)
      2. mask_hook: "psql -c 'UPDATE users SET email = id || '@test.local''"
```

Ensures PR environments never contain real PII even though they clone from
staging.

### Warm pools

Pre-create N environments from the latest staging snapshot. When a PR opens,
grab one from the pool instead of provisioning fresh. Reduces spin-up from ~13s
to ~3s (just run migrations + restart services with new image).

```
warm_pool:
  size: 3
  base_snapshot: staging (refreshed nightly)
  pre_provisioned: [phases 1-2 already done]
  on_claim: run phases 3-5 only
```

### Resource quotas

PR environments have automatic limits:

```
pr_environment_policy:
  max_concurrent: 10
  max_lifetime: 48h (auto-destroy)
  resource_profile: small (reduced CPU/memory per service)
  zfs_quota_per_clone: 10GB
  autoscaler: disabled (always min replicas)
  cron_jobs: disabled
```

### Scope: shared vs isolated

Not everything needs cloning. Canvas objects declare their scope:

| Scope | Meaning | Example |
|-------|---------|---------|
| `isolated` | Cloned/created per PR environment | Database, application services |
| `shared` | Reuse the staging instance | Auth service, feature flag service |
| `disabled` | Not present in PR environments | CronJobs, log drains, alert rules |

A frontend PR environment might use `shared` scope for the backend API:

```
ExternalService "backend-api":
  source: staging/api
  scope: shared
  outputs: [API_URL]
```

### Environment composition

A PR environment can compose from multiple canvases:

```
Backend canvas: api, worker, postgres, redis
Frontend canvas: next.js app, CDN config
Shared canvas: auth service, feature flags

Frontend PR: clones frontend canvas + references backend staging (shared)
Backend PR: clones backend canvas + references frontend staging (shared)
Full-stack PR: clones both canvases
```

### Agent / CI integration

Agents (AI or CI) interact via API:

```
POST /environments
  { canvas_id, branch, pr_number, overrides: { ... } }
  → returns { environment_id, urls: { api: "...", frontend: "..." } }
  → ~13s later, fully ready

GET /environments/:id/status
  → { phase: "ready", services: [...], urls: {...} }

DELETE /environments/:id
  → tears down, runs on_destroy hooks

POST /environments/:id/deploy
  → re-deploys with latest images (PR update)
```

Overrides let an agent customize the environment:

```json
{
  "overrides": {
    "api": { "image": "api:pr-142-sha-abc123" },
    "env": { "DEBUG": "true", "LOG_LEVEL": "trace" }
  }
}
```

### Branch tracking

PR environments track their source commit. On new pushes:
- Only rebuild images whose source changed
- Only re-run migrations if schema files changed
- Services with unchanged images just restart with new env

The agent detects changed paths (like a CI `paths` filter) to minimize work.

## Eject story

### What the user keeps

- All running services (unchanged)
- Full Corrosion state (specs, revisions, slots, machines)
- CLI access to everything
- WireGuard mesh (unchanged)

### What they lose

- Dashboard UI
- Canvas objects (shared envs, autoscalers, visual layout)
- Team/RBAC management, audit log history
- Alerting/notifications
- The agent process

### Migration path

Export canvas as `DeployManifest` JSON files (one per namespace). The compiled
output is identical to what the CLI accepts. `ployz deploy -f manifest.json`
from a CI pipeline replaces the dashboard entirely.