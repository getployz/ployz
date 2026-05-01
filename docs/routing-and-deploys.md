# Routing & Deploys

How services get deployed onto machines and how traffic finds its way to them.

## Data Model

The distributed store holds all the state that drives both routing and
deployment. The tables divide into three conceptual groups:

**Mesh infrastructure** — the machine registry and join tokens. Who's in the mesh
and what their keys and overlay IPs are.

**Service versioning & placement** — an append-only ledger of service spec versions
(content-addressed, immutable), a mutable head pointer per service (which version is
active), and slot records that bind replicas to machines. This is the scheduling layer.

**Runtime state** — per-instance lifecycle records (phase, readiness, drain state, ports)
and deploy lifecycle tracking. This is what routing reads to decide who's healthy.

### Design Rationale

Deploy commits are append-only authority events. Readers project those commits into
routing state in stream order, which keeps deploy visibility atomic without exposing
partially-published release/volume state.

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
| Replicated(N) | N slots, distributed across available machines. `replicated(1)` is the single-instance case. |
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

### Target Apply Model

The apply phase is an operator-triggered distributed operation with NATS as the
coordination authority:

**Lock** — The coordinator acquires one namespace lease in NATS KV
(`locks.deploy.<namespace>`) using CAS. The lease carries owner, nonce, and expiry.
Participants do not own the deploy lock.

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

**Commit** — A single immutable deploy commit publishes the new release, volume, and
deploy envelope. This is the point of no return. NATS persists the event and readers
project it atomically.

**Cleanup** — Old instances are drained (marked unhealthy so routing drops them) then
removed. If cleanup fails, the deploy enters CleanupPending — the new version is live
but old containers linger. This is a recoverable state, not a failure.

### Remote Deploy Protocol

The target protocol for small participant commands is NATS request/reply on
`node.<machine>.cmd.deploy.*` subjects. Commands include inspect namespace, start
candidate, drain instance, and remove instance. No-responder and timeout errors
fail the foreground deploy operation.

The implementation models participants as explicit command targets, not long-lived
sessions. Each runtime action is its own NATS request/reply command. Namespace lock
ownership lives in NATS KV and is held only by the deploy coordinator; participant
command subscriptions do not carry authority beyond handling the requested local
runtime action.

---

## Routing

### Snapshot Plus Durable Batches

All routing decisions start from one snapshot of the distributed store's routing
tables. After the snapshot, live consumers apply ordered routing event batches from
the `routing_events` JetStream stream.

The snapshot is the catch-up boundary. If a process restarts or loses its local
projection, it subscribes again, reads a fresh snapshot, then receives only new
batches (`DeliverPolicy::New`) for that subscription.

### Subscription Model

Routing event consumers declare their durability explicitly:

- **Durable subscriptions** are used by long-lived service projections such as
  gateway and DNS. Their consumer ids are stable per machine, so each process
  receives every routing batch independently.
- **Temporary subscriptions** are used by live watch clients such as
  `RuntimeSubscribe` and startup readiness probes. They are cleaned up when the
  watcher closes.

Properties:

- Atomic batches — related routing events are published as one JetStream atomic
  batch, and consumers ack the batch only after applying it.
- Per-consumer cursors — gateway, DNS, runtime watch, and readiness probes do not
  share a cursor.
- Graceful degradation — projection errors log and keep the previous snapshot;
  a restart rebuilds from durable state.

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
namespace it belongs to. Overlay workloads are configured to use `ployz-dns` by default,
so newly deployed overlay services can resolve services in their own namespace by short
name (`db`) without knowing the namespace. Cross-namespace queries use the full form
(`db.prod.ployz.internal`).

TTL is always 0 — clients re-query every time, ensuring they never cache routes to
drained instances.

---

## End-to-End: Deploy to First Request

```
1. User runs `ployzd deploy manifest.toml`

2. Preview: diff manifest against current state
   -> "api" needs 3 new slots on machines A, B, C

3. Apply:
   a. Acquire one NATS deploy lease for the namespace
   b. Register revision (content-addressed, idempotent)
   c. Start containers on A, B, C, wait for readiness probes
   d. Atomic commit — single transaction flips all routing pointers
   e. NATS replicates the commit to storage candidates and mirrors

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
