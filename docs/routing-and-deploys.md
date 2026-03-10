# Routing & Deploys

How services get deployed onto machines and how traffic finds its way to them.

## Data Model

The distributed store (Corrosion) holds all the state that drives both routing and
deployment. The tables divide into three conceptual groups:

**Mesh infrastructure** — the machine registry and join tokens. Who's in the mesh,
what their keys and overlay IPs are, when they last heartbeated.

**Service versioning & placement** — an append-only ledger of service spec versions
(content-addressed, immutable), a mutable head pointer per service (which version is
active), and slot records that bind replicas to machines. This is the scheduling layer.

**Runtime state** — per-instance lifecycle records (phase, readiness, drain state, ports)
and deploy lifecycle tracking. This is what routing reads to decide who's healthy.

### Design Rationale

Corrosion replicates entire rows, so fields that change together live in the same table.
Instance phase, readiness, and drain state all change during deploy transitions — they
belong together. Service head pointers change atomically at commit — they're separate
from the append-only revision ledger. This isn't normalized for query convenience; it's
shaped for replication efficiency.

---

## Deployments

### Concepts

A **deploy** applies a manifest (a list of service specs) to a namespace. Each spec is
content-hashed into a revision. The deploy engine diffs desired vs current state, starts
new containers, waits for readiness, atomically commits the new routing state, then
cleans up old containers.

### Slot Model

Each service replica is represented by a **slot** — a stable identifier bound to a
machine. Placement depends on the service's strategy:

| Strategy | Behavior |
|----------|----------|
| Singleton | One slot, pinned to a single machine (sticky across redeploys) |
| Replicated(N) | N slots, distributed across available machines |
| Global | One slot per machine in the mesh |

A slot points to an **active instance** (a Docker container). During a deploy, new
instances run alongside old ones. The slot pointer flips atomically at commit time —
there is no window where traffic hits a half-deployed state.

### Deploy Lifecycle

```
Planning -> Applying -> Committed
                    \-> Failed
                    \-> CleanupPending (committed but old instances failed to remove)
```

### How Apply Works

The apply phase is a distributed coordination protocol:

**Lock** — The coordinator acquires namespace locks on all participant machines over TCP.
Locks are tied to the TCP connection lifetime — if the coordinator crashes, locks release
automatically.

**Discover** — Reconcile live container state with the store on every participant.
Orphaned containers get re-registered. This recovers from any prior inconsistency.

**Revalidate** — Recompute the plan while holding locks. If machines changed between
preview and apply (e.g. one went down), abort with a retry error rather than deploying
to a stale plan.

**Register** — Upsert immutable, content-addressed revision records. Duplicate publishes
are no-ops by design.

**Create** — For each slot in the plan: reuse unchanged instances, or start new candidate
containers and wait for readiness probes (TCP/HTTP/exec). Readiness is non-negotiable —
nothing enters routing until it passes.

**Commit** — A single Corrosion transaction flips all head pointers, slot assignments,
and deploy state atomically. This is the point of no return. Corrosion replicates the
transaction to every machine in the mesh.

**Cleanup** — Old instances are drained (marked unhealthy so routing drops them) then
removed. If cleanup fails, the deploy enters CleanupPending — the new version is live
but old containers linger. This is a recoverable state, not a failure.

### Remote Deploy Protocol

The coordinator talks to participant machines over line-delimited JSON/TCP. The protocol
is request-response: the coordinator sends commands (open session, inspect namespace,
start candidate, drain/remove instance, close session) and the participant responds.

The namespace lock is implicit in the session — opening a session acquires it, closing
(or disconnecting) releases it. No explicit heartbeat needed.

---

## Routing

### Snapshot-Based Routing

All routing decisions derive from a single snapshot of the distributed store's routing
tables. There are no incremental updates — when anything changes, the full snapshot
reloads and the routing projection rebuilds from scratch.

This is intentionally simple. The snapshot is small (it's metadata, not payloads), and
full rebuilds are cheap compared to the complexity of incremental consistency.

### Invalidation Model

When any routing-related table is written to, the store broadcasts an invalidation signal.
Subscribers (gateway, DNS) debounce for 100ms, drain buffered signals, then reload.

Properties:
- Non-blocking and lossy — extra signals are dropped because the next one triggers a full reload anyway
- Debounced — rapid writes during a deploy commit batch into a single reload
- Gracefully degrading — projection errors log and keep the previous snapshot

### Gateway (HTTP/TCP Proxy)

The gateway projects the routing snapshot into a set of HTTP routes (host + path → backends)
and TCP routes (port → backends).

An instance is **routable** when it's ready, not draining, has no errors, has an overlay IP,
and its slot/machine/revision all match current records. This is a strict filter — any
ambiguity means the instance is excluded.

Request handling: match Host header and path (longest prefix wins, explicit hosts before
wildcards), select a backend via round-robin, proxy over the WireGuard overlay. On upstream
failure, retry with a different backend.

The snapshot is shared via double-Arc — readers clone an Arc and release immediately, so
request handling never blocks on snapshot updates.

### DNS

DNS projects the routing snapshot into service name → IP mappings. Only ready, non-draining
instances with overlay IPs are included.

Namespace derivation is implicit: a container's source IP is looked up to find which
namespace it belongs to. This means containers can resolve services in their own namespace
by short name (`db`) without knowing the namespace. Cross-namespace queries use the full
form (`db.prod.ployz.internal`).

TTL is always 0 — clients re-query every time, ensuring they never cache routes to
drained instances.

---

## End-to-End: Deploy to First Request

```
1. User runs `ployz deploy manifest.toml`

2. Preview: diff manifest against current state
   -> "api" needs 3 new slots on machines A, B, C

3. Apply:
   a. Lock namespace on A, B, C
   b. Register revision (content-addressed, idempotent)
   c. Start containers on A, B, C, wait for readiness probes
   d. Atomic commit — single transaction flips all routing pointers
   e. Corrosion replicates to all machines

4. Gateway reloads:
   - Loads snapshot, projects routes, finds 3 healthy backends
   - Builds routing table with backends

5. DNS reloads:
   - Maps "api" -> overlay IPs of healthy instances

6. First request arrives:
   - Host header matches api route
   - Round-robin selects a backend
   - Proxies over the WireGuard overlay

7. Old instances drained and removed
```
