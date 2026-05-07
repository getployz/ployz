# Routing & Deploys

How services get deployed onto machines and how traffic finds its way to them.

## Data Model

The distributed store holds all the state that drives both routing and
deployment. The routing collections divide into three conceptual groups:

**Mesh infrastructure** — machine membership records and join tokens. Who's in the mesh
and what their keys and overlay IPs are.

**Service versioning & placement** — append-only deploy commits record service
spec versions (content-addressed, immutable), the active version per service,
region placement policy, and slot records that bind replicas to machines.
This is the scheduling layer.

**Runtime state** — per-instance lifecycle records (phase, readiness, drain state, ports)
and deploy lifecycle tracking. This is what routing reads to decide who's healthy.

### Design Rationale

Deploy commits are append-only events in the owning NATS authority. Readers
project those commits into routing state in stream order, which keeps deploy
visibility atomic without exposing partially-published release/volume state.
Regions are placement and routing groups; they are not necessarily independent
write authorities. In the MVP, most deploys can be global while durable deploy
truth still commits to one home/data authority.

---

## Deployments

### Concepts

A **deploy** applies a manifest (a list of service specs) to a namespace. Each spec is
content-hashed into a revision. The deploy engine compares desired vs current state,
starts new containers, waits for readiness, appends one immutable deploy commit,
publishes the derived routing facts, then cleans up old containers.

### Slot Model

Each service replica is represented by a **slot** — a stable identifier bound to a
machine. Placement depends on the service's strategy:

| Strategy | Behavior |
|----------|----------|
| Replicated(N) | N slots, distributed across available machines. `replicated(1)` is the single-instance case. |
| Global | Slots across every eligible region, with per-region replica policy and live capacity offers deciding the actual machines. |

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
coordination substrate and the namespace's owning authority as the durable write
authority:

**Lock** — The coordinator acquires one namespace lease in NATS KV
(`locks.deploy.<namespace>`) using CAS. The lease carries owner, nonce, and expiry.
Participants do not own the deploy lock.

**Discover** — Reconcile live container state with the store on every participant.
Orphaned containers get re-registered. This recovers from any prior inconsistency.

**Probe placement** — For global or regional deploys, the coordinator sends
bounded NATS request/reply probes to eligible regions/machines: "I need N
replicas of this revision with these resources." Machines reply with expiring
capacity offers. Offers are live signals, not durable ownership.

**Revalidate** — Recompute the plan while holding locks. If machines changed between
preview and apply (e.g. one went down), abort with a retry error rather than deploying
to a stale plan.

**Register** — Build immutable, content-addressed revision payloads. They are
included in the deploy commit rather than written as separate mutable records.

**Create** — For each selected slot in the plan: reuse unchanged instances, or
start new candidate containers and wait for readiness probes (TCP/HTTP/exec).
Readiness is non-negotiable — nothing enters routing until it passes.

**Commit** — A single immutable deploy commit publishes the deploy envelope,
revisions, releases, and volume changes. This is the point of no return. NATS
persists the event and readers project it atomically; routing notifications are
one ordered JetStream message per derived routing fact.

**Cleanup** — Old instances are drained (marked unhealthy so routing drops them) then
removed. If cleanup fails, the deploy enters CleanupPending — the new version is live
but old containers linger. This is a recoverable state, not a failure.

### Remote Deploy Protocol

The target protocol for small participant commands is NATS request/reply on
per-machine command subjects. The current implementation uses
`node.<machine>.cmd.deploy.*`; the long-term subject shape moves those under the
installation substrate/authority planes described in `nats_future.md`. Commands
include inspect namespace, placement probe, prepare/reserve, start candidate,
drain instance, and remove instance. No-responder and timeout errors fail the
foreground deploy operation.

The implementation models participants as explicit command targets, not long-lived
sessions. Each runtime action is its own NATS request/reply command. Namespace lock
ownership lives in NATS KV and is held only by the deploy coordinator; participant
command subscriptions do not carry authority beyond handling the requested local
runtime action.

---

## Routing

### Snapshot Plus Ephemeral Events

All routing decisions start from one snapshot of the distributed store's routing
collections. After the snapshot, live consumers apply ordered routing events from the
`routing_events` JetStream stream.

Subscription setup reads the routing event stream's next sequence, loads a fresh
snapshot, then starts an ephemeral memory-backed consumer from that same sequence
(`DeliverPolicy::ByStartSequence`). Events that raced into the snapshot may be
delivered again; routing events are idempotent facts, so replaying them is safe
and avoids a separate manual catch-up phase.
If a process restarts, a watcher closes, or routing-view freshness becomes
uncertain, the process discards the local view and repeats that sequence.

### Subscription Model

Routing event consumers are all temporary NATS consumers. Gateway, DNS, runtime
watch, and readiness probes rebuild from durable state first, then consume only
the events that occur after that observed stream sequence. The consumer's
`max_ack_pending` matches the process bridge-channel capacity, and idle
heartbeats turn a broken delivery path into an explicit subscription failure.

Properties:

- Plain event stream — each routing fact is one JetStream message, and
  consumers ack each event only after applying it. Facts are upserts or removals;
  old/new database-style diffs are intentionally not part of the event stream.
- No durable cursors — watchers do not leave server-side routing cursor state
  behind after exit or daemon crash.
- Visible freshness loss — projection errors surface as subscription failures;
  consumers reload from durable state instead of silently continuing from a stale
  event stream.

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
   -> "api" wants global placement, with 2 replicas in us-west and 1 in sin

3. Apply:
   a. Acquire one NATS deploy lease for the namespace
   b. Probe eligible regions/machines for live capacity offers
   c. Build content-addressed revision payloads for the commit
   d. Prepare selected machines, start containers, wait for readiness probes
   e. Append one immutable deploy commit
   f. Publish one routing event per derived routing fact
   g. NATS replicates the commit and routing events to the selected stream peers
      in the owning home/data authority

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
