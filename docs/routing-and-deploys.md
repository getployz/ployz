# Routing & Deploys

How services get deployed onto machines and how traffic finds its way to them.

## Data Model

Six tables in the Corrosion distributed store hold the state that drives both
systems. They divide into three groups:

**Mesh infrastructure:**
- `machines` — machine registry (WireGuard keys, overlay IPs, heartbeats)
- `invites` — one-time join tokens

**Service versioning & placement:**
- `service_revisions` — append-only ledger of service spec versions
- `service_heads` — mutable pointer: (namespace, service) -> current revision
- `service_slots` — desired placement: one row per replica, binding a slot to a
  machine and an active instance

**Runtime state:**
- `instance_status` — per-container lifecycle (phase, readiness, drain state, ports)
- `deploys` — deployment lifecycle tracking

### Why so many columns?

Each table serves a different audience and update cadence. Corrosion replicates
entire rows, so co-locating fields that change together (e.g. instance phase +
ready + drain_state) avoids unnecessary row splits. The columns break down as:

| Category | Examples | Why |
|----------|----------|-----|
| Identity | instance_id, slot_id, machine_id | Join keys across tables |
| Versioning | revision_hash, manifest_hash | Content-addressed deduplication |
| Network | overlay_ip, backend_ports_json, endpoints | Routing needs socket addresses |
| Lifecycle | phase, ready, drain_state, status, participation | Filtering for routability |
| Audit | deploy_id, created_by, updated_at, started_at | Traceability across deploys |

---

## Deployments

### Concepts

A **deploy** applies a `DeployManifest` (a list of `ServiceSpec`s) to a
namespace. Each spec is content-hashed into a `revision_hash`. The deploy engine
computes a diff between desired and current state, starts new containers, waits
for readiness, atomically commits the new routing state, then cleans up old
containers.

### Slot Model

Each service replica is represented by a **slot** — a stable identifier bound to
a machine. Placement depends on the service's `placement` strategy:

| Strategy | Slots |
|----------|-------|
| Singleton | One slot, pinned to a single machine (sticky across redeploys) |
| Replicated(N) | N slots, distributed round-robin across available machines |
| Global | One slot per machine, using the machine ID as the slot ID |

A slot points to an **active instance** (a Docker container). During a deploy,
new instances are started alongside old ones; the slot pointer is flipped
atomically at commit time.

### Deploy Lifecycle

```
Planning -> Applying -> Committed
                    \-> Failed
                    \-> CleanupPending (committed but old instances failed to remove)
```

### Deploy Phases (apply)

**1. Lock acquisition**

The coordinator acquires a namespace lock locally, then connects to all
participant machines over TCP and sends `LockAcquire` RPCs. A background
heartbeat (5s interval) keeps remote locks alive. If any lock fails, the deploy
aborts.

**2. Instance discovery**

The coordinator calls `adopt_instances()` locally (reconciles Docker containers
with the store) and sends `InspectNamespace` RPCs to each remote machine. This
recovers from any state inconsistency — orphaned containers are re-registered.

**3. Revalidation**

The preview is recomputed while holding locks. If the participant set changed
(e.g. a machine went down between preview and apply), the deploy aborts with a
retry error.

**4. Revision registration**

For each service in the manifest, a `ServiceRevisionRecord` is upserted
(INSERT OR IGNORE). Revisions are immutable and content-addressed, so
duplicate publishes are no-ops.

**5. Instance creation**

For each slot in the plan:
- If the slot is unchanged (same machine, same revision): reuse the existing instance.
- Otherwise: start a new candidate container (local or remote via `StartCandidate` RPC),
  then wait for the readiness probe to pass (TCP/HTTP/exec, 15s timeout).
  Write the `InstanceStatusRecord` to the store.

**6. Atomic commit**

`commit_deploy()` executes a single Corrosion transaction that:
1. Deletes old `service_heads` and `service_slots` for touched services
2. Inserts new heads pointing to the new revision
3. Inserts new slots pointing to the new instances
4. Inserts/updates the `deploys` record with state=Committed

This is the atomic boundary — all routing tables flip together. Corrosion
replicates the transaction to every machine in the mesh.

**7. Cleanup**

After commit, the coordinator removes old instances:
- Mark as draining (phase=Draining, ready=false)
- Remove the Docker container
- Delete the `InstanceStatusRecord`

Participants pick up the new routing state through Corrosion's eventual
consistency — the invalidation subscription triggers a snapshot reload once
the committed rows replicate.

If any cleanup fails, the deploy enters `CleanupPending` instead of staying
`Committed`.

### Remote Deploy Protocol

Coordinator-participant model over line-delimited JSON/TCP:

| RPC | Purpose |
|-----|---------|
| LockAcquire | Acquire namespace lock on participant |
| LockHeartbeat | Keep remote lock alive (5s interval) |
| InspectNamespace | List local instances for reconciliation |
| StartCandidate | Create container + wait for readiness |
| DrainInstance | Mark instance as draining |
| RemoveInstance | Delete container + store record |
| Unlock | Release namespace lock |

---

## Routing

### RoutingState

All routing decisions derive from a single snapshot:

```
RoutingState {
    revisions:  Vec<ServiceRevisionRecord>,   // what each version's spec looks like
    heads:      Vec<ServiceHeadRecord>,        // which version is active per service
    slots:      Vec<ServiceSlotRecord>,        // where replicas are placed
    instances:  Vec<InstanceStatusRecord>,     // runtime state of each container
}
```

### Invalidation Model

When any routing-related table is written to, the store broadcasts an
invalidation signal (`()`) to all subscribers. Subscribers (gateway, DNS) receive
the signal, debounce for 100ms, drain any buffered invalidations, then reload
the full `RoutingState` and rebuild their projection.

Properties:
- Non-blocking: `try_send` on a bounded channel (capacity 64)
- Lossy on overflow: extra invalidations are dropped (next signal triggers full reload anyway)
- Debounced: 100ms window batches rapid writes into a single reload
- Graceful degradation: projection errors log a warning and keep the previous snapshot

### Gateway (HTTP/TCP Proxy)

The gateway (Pingora-based) projects `RoutingState` into a `GatewaySnapshot`:

```
GatewaySnapshot {
    http_routes: Vec<HttpRouteView>,   // host + path_prefix -> backends
    tcp_routes:  Vec<TcpRouteView>,    // listen_port -> backends
}
```

**Projection rules:**

1. For each `ServiceHeadRecord`, resolve to its `ServiceRevisionRecord` via
   `current_revision_hash`
2. Parse the revision's `spec_json` to get route definitions (hostnames, paths, ports)
3. For each route, collect backends from instances that pass the routability filter
4. Detect conflicts (duplicate host+path or duplicate listen_port)

**Routability filter** — an instance is routable when ALL of:
- `phase == Ready` and `ready == true`
- `drain_state == None`
- `error` is empty
- `overlay_ip` is set
- Instance's slot, machine, and revision all match the current slot record

**Request flow:**
1. Extract Host header and path from request
2. Match against `http_routes` (sorted by specificity: longest path first, explicit hosts before wildcards)
3. Select backend via per-route round-robin
4. Proxy to `overlay_ip:backend_port`
5. On upstream failure: mark instance as failed, retry with different backend

The snapshot is shared via a double-Arc pattern (`Arc<RwLock<Arc<GatewaySnapshot>>>`) —
readers clone the inner Arc and release the lock immediately, so request handling
never blocks on snapshot updates.

### DNS

The DNS server projects `RoutingState` into a `DnsSnapshot`:

```
DnsSnapshot {
    services:        HashMap<(Namespace, String), Vec<Ipv4Addr>>,  // service -> IPs
    ip_to_namespace: HashMap<Ipv4Addr, Namespace>,                 // reverse lookup
    service_names:   HashMap<Namespace, Vec<String>>,              // list services
}
```

Only ready, non-draining instances with an overlay IP are included.

**Query resolution:**

| Query | Type | Result |
|-------|------|--------|
| `db` | A | Resolve using caller's namespace (derived from source IP) |
| `db.ployz.internal` | A | Same as above (implicit namespace) |
| `db.prod.ployz.internal` | A | Explicit namespace lookup |
| `_services.ployz.internal` | TXT | List all service names in caller's namespace |
| `_services.prod.ployz.internal` | TXT | List services in explicit namespace |

Namespace derivation: the caller's source IP is looked up in `ip_to_namespace`
(built from instance overlay IPs). This means containers can resolve services in
their own namespace by short name, without knowing the namespace explicitly.

TTL is always 0 — clients re-query every time, ensuring they never route to
drained instances.

---

## End-to-End: Deploy to First Request

```
1. User runs `ployz deploy manifest.toml`

2. Preview: diff manifest against current heads/slots
   -> "api" needs 3 new slots on machines A, B, C

3. Apply:
   a. Lock namespace on A, B, C
   b. Register ServiceRevisionRecord (content-addressed, idempotent)
   c. Start containers on A, B, C, wait for readiness probes
   d. commit_deploy() — atomic transaction:
      - service_heads("api") -> new revision_hash
      - service_slots("api", "slot-0001") -> machine A, instance X
      - service_slots("api", "slot-0002") -> machine B, instance Y
      - service_slots("api", "slot-0003") -> machine C, instance Z
   e. Corrosion replicates committed rows to all machines
   f. Routing invalidation subscriptions fire on each node

4. Gateway sync loop wakes up:
   - Loads RoutingState
   - Projects: api's revision -> spec -> routes -> host "api.example.com"
   - Finds 3 routable instances (X, Y, Z) with overlay IPs
   - Builds GatewaySnapshot with backends

5. DNS sync loop wakes up:
   - Loads RoutingState
   - Maps "api" -> [10.210.1.5, 10.210.2.5, 10.210.3.5]

6. First request arrives at gateway:
   - Host: api.example.com -> matches api route
   - Round-robin selects instance X on machine A
   - Proxies to 10.210.1.5:8080 over the WireGuard overlay

7. Old instances (if any) drained and removed
```