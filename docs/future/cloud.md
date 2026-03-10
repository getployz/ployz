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