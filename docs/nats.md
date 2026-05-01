# NATS

Reference for the NATS substrate underneath ployz, with emphasis on features
added in 2.11+ that aren't well-represented in older docs or training data.
Covers the primitives, the recent additions, the topology options, the
coordination patterns we build on top, and forward-looking design notes for
ployz.

This is a working reference. The deployment specifics that change as the
implementation evolves live in code; the conceptual model and the NATS
features we depend on live here.

For the product target and failure semantics, read
[`docs/nats-native-control-plane.md`](nats-native-control-plane.md). For the
system-test plan, read [`docs/testing/e2e.md`](testing/e2e.md).

## Version baseline

ployz pins `nats:2.14-alpine` in `crates/ployzd/assets/built_in_images.toml`.

Highlights by version:

- **2.10** baseline. JetStream KV, streams, consumers, mirrors, sources,
  domains, leafnodes.
- **2.11** per-message TTLs (`Nats-TTL` header) on streams and KV. Subject
  delete markers (`Nats-Marker-Reason`). Batch retrieval of stream messages.
  Stream ingest rate limiting (`max_buffered_size`, `max_buffered_msgs`).
  Leader sync before serving — fixes a class of leader-changeover races.
- **2.12** mirror promotion to primary (real DR path). Better protection
  against empty-state leader elections. Connect backoff on routes/gateways.
  PROXY protocol v1/v2 for client connections. The 2.12.6 batch fixed 9
  CVEs across MQTT, leafnodes, WebSockets, JetStream, TLS, publishing.
- **2.14** fast-ingest batch publishing. Repeating and cron-based message
  schedules via `Nats-Schedule` header. Consumer reset API (rewind without
  delete). Async stream state snapshots. **Leafnode runtime configuration
  reload** — adding/removing remotes no longer requires restart.

The pin is currently a tag, not a digest. Six months from prod is fine for
that; before any production cut, switch to a 2.14.x digest pin matching the
pattern of the other built-in images.

## Core primitives

NATS speaks four base patterns over **subjects** — dotted hierarchical
names like `node.m4.cmd.deploy.start_candidate`:

- **Pub/sub.** Publishers send to a subject; subscribers matching the
  subject (with optional `*` and `>` wildcards) receive a copy.
- **Request/reply.** A request includes an inbox subject for the reply.
  The library handles the ephemeral inbox subscription and a timeout.
  No-responder detection is built in: if zero subscribers match the
  request subject, the broker tells the publisher immediately.
- **Queue groups.** N subscribers on the same subject sharing a queue
  group name; each message goes to exactly one of them, round-robin.
  Used for load balancing across stateless workers.
- **Headers.** Arbitrary key/value pairs attached to a message. NATS
  reserves several headers (`Nats-TTL`, `Nats-Schedule`, `Nats-Msg-Id`,
  `Nats-Expected-Last-Sequence`, etc.) for protocol-level features.

Wildcards: `*` matches one token, `>` matches one or more. `cmd.*.start`
matches `cmd.foo.start` but not `cmd.foo.bar.start`. `cmd.>` matches both.

## JetStream

JetStream is the durable layer. Adds streams, consumers, KV, and Object
Store on top of core NATS. Backed by Raft for replicas > 1.

### Streams

A stream captures messages on configured subjects and persists them.

Key fields:

- `name` — identifier.
- `subjects` — list of subject patterns the stream listens on.
- `retention` — `Limits` (default), `WorkQueue` (consumed-once), or
  `Interest` (drop when no consumer is interested).
- `storage` — `File` or `Memory`.
- `num_replicas` — Raft replicas (1, 3, or 5; never 2).
- `discard` — `Old` or `New` when limits hit.
- `max_age`, `max_messages`, `max_bytes`, `max_messages_per_subject` —
  retention limits.
- `duplicate_window` — dedup window for `Nats-Msg-Id` (default 2 min).
- `allow_direct` — enable `direct_get` for fast key-style reads.
- `allow_atomic_publish` — accept atomic batch publishes (2.12+).
- `subject_delete_marker_ttl` — emit a delete marker when MaxAge expires
  the last message for a subject (2.11+).

Retention modes:

| Retention   | Behavior                                                   | Use                          |
|-------------|------------------------------------------------------------|------------------------------|
| `Limits`    | Keep until limits hit, then evict by `discard` policy      | Event log, audit, replay     |
| `WorkQueue` | Message removed after a consumer acks it                   | Job dispatch, single-worker  |
| `Interest`  | Removed when no consumer expresses interest in it          | Fan-out without persistence  |

Deduplication: publishing with `Nats-Msg-Id: foo` makes the broker drop
duplicate publishes within `duplicate_window`. Cheap idempotency.

OCC: publishing with `Nats-Expected-Last-Subject-Sequence: 0` makes the
publish fail if any earlier message exists on that subject. Equivalent to
"create only" — used for committing immutable facts at known IDs.

### Consumers

A stream is just storage; consumers are how readers traverse it.

- **Push consumers** — server delivers to a subject the consumer is
  subscribed to.
- **Pull consumers** — consumer requests batches with explicit ack. Better
  for backpressure, exactly-one-worker semantics, and rate-limited workers.
- **Durable** — server remembers the consumer's position across reconnects.
  Identified by name.
- **Ephemeral** — disappears when the connection drops.

Key fields:

- `deliver_policy` — `All`, `New`, `ByStartSequence`, `ByStartTime`,
  `LastPerSubject`.
- `ack_policy` — `Explicit`, `None`, `All` (acks all prior).
- `ack_wait` — redeliver if no ack in this window.
- `max_deliver` — give up after N attempts.
- `filter_subject` / `filter_subjects` — only deliver matching messages.
- `replay_policy` — `Instant` or `Original` (preserve original timing).

The 2.14 **consumer reset API** rewinds a durable consumer's position
without recreating it. Useful for re-running projections after a code
change without losing the consumer's identity or subscribers.

### Atomic batch publish (2.12+)

A stream with `allow_atomic_publish: true` accepts a batch of messages
that the broker commits atomically — all or none. Set `Nats-Batch-Id`
on each message in the batch and `Nats-Batch-Commit` on the final one.

```rust
// async-nats sketch
js.publish_with_headers(subject, headers_with_batch_id, payload).await?;
js.publish_with_headers(subject, headers_with_batch_id, payload).await?;
js.publish_with_headers(subject, headers_with_batch_commit, payload).await?;
```

Useful when you have multi-message logical operations that must either all
land or all fail. We don't need it for `DeployCommit` (single self-
contained envelope), but it's the right tool if a future design splits a
commit into multiple events.

### Scheduled messages (2.14+)

`Nats-Schedule` header makes the broker hold a message and emit it at a
future time. Two formats: an RFC3339 timestamp for one-shot, or a cron
expression for repeating.

```
Nats-Schedule: 2026-08-15T03:00:00Z
Nats-Schedule: cron 0 3 * * *
```

This is the right primitive for cert renewal: schedule the renewal job at
the cert's renewal-due time and let NATS fire it. Replaces the leader-
elected ticker pattern.

### Batch retrieval (2.11+)

Stream messages can be fetched in batches via the JetStream API rather
than one `direct_get` per sequence. Cuts cold-start replay from N
round-trips to a small number of round-trips.

### Direct gets

`allow_direct: true` lets reads bypass the consumer machinery and hit the
storage layer directly. Used by KV under the hood.

## Key-Value

KV is a thin abstraction over a stream with `MaxMsgsPerSubject = 1` (or
configurable history). Operations:

- `get(key)` — direct read of latest value.
- `put(key, value)` — overwrite.
- `create(key, value)` — atomic create, fails if key exists. **CAS for
  pessimistic locks.**
- `update(key, value, expected_revision)` — atomic update, fails if
  revision doesn't match. **CAS for optimistic concurrency.**
- `delete(key)` / `delete_expect_revision(key, rev)` — remove; either
  unconditional or revision-checked.
- `purge(key)` — delete and clear history.
- `entry(key)` — full entry with revision number.
- `keys()`, `entries()` — list iterators.
- `watch_all()`, `watch(key_pattern)` — snapshot + stream of `Operation::Put` /
  `Operation::Delete` events with revision numbers.
- `history(key)` — returns previous values up to `history` limit.

Per-message TTL (2.11+) on KV: write with `Nats-TTL: <duration>` to set a
per-key expiry. Critically, the TTL **applies to the new revision**, so
`update_with_ttl` (where the API exposes it) refreshes the lease window —
the right primitive for renewable leases. async-nats had been behind on
this; check the current version.

Limit markers on KV: when a value expires by TTL, the broker can emit a
delete marker with `Nats-Marker-Reason: MaxAge` so watchers see why a
key disappeared.

History: `kv::Config { history: N }` keeps the last N revisions of each
key. `history > 1` is rarely useful for lock-style state; useful for
audit-style state where rollback or "what was the value yesterday" matters.

## Object Store

Stream-backed blob storage. Use when values exceed comfortable KV size
(KV default value cap is 1 MiB). Split into chunks under the hood, each
object addressed by name with metadata.

Use cases for ployz:

- TLS PEM material per `(hostname, not_after)` — cert chains can exceed
  KV size limits with SAN-heavy certs.
- Compaction snapshot blobs once `deploy_commits` history grows large
  enough that full replay becomes expensive.

## Mirrors and Sources

The non-quorum redundancy primitives. **Important: these are not Raft
replicas.** They're async, eventually consistent, lag-bounded copies.

### Mirrors

A mirror stream pulls all messages from a single source stream. Read-only.
Lag is typically single-digit milliseconds in healthy networks; grows
under load or partition.

Config sketch:

```json
{
  "name": "deploy_commits_local",
  "mirror": {
    "name": "deploy_commits",
    "external": { "api": "$JS.hub.API" }
  },
  "storage": "file"
}
```

The `external.api` field is what makes this a cross-domain mirror — the
mirror lives in one JetStream domain and pulls from a stream in another.
Without `external`, the source has to be in the same domain.

KV mirrors work the same way (KV is a stream).

### Mirror promotion to primary (2.12+)

A mirror can be promoted to be the primary. Disaster-recovery move: if the
hub cluster is permanently lost, promote a mirror, redirect writes there.
Before 2.12 this required manual data extraction and re-publication.

### Sources

Like a mirror but multi-input. One stream pulls from N source streams,
optionally filtering by subject and remapping subjects on ingest:

```json
{
  "name": "all_namespaces_view",
  "sources": [
    { "name": "deploy_commits", "filter_subject": "deploy_commits.prod.>" },
    { "name": "deploy_commits", "filter_subject": "deploy_commits.staging.>" }
  ]
}
```

Useful for fan-in patterns. Less obvious application to ployz unless we
end up sharding by namespace.

## Topology

Three primitives compose the cluster shape:

### Routes

Full-mesh peer connections between nats-server instances in the same
**cluster**. Carry both core NATS and JetStream Raft consensus traffic.

Configured under `cluster {}`:

```
cluster {
  name: ployz-alpha
  listen: [fd00::1]:6222
  routes: [
    "nats://[fd00::2]:6222",
    "nats://[fd00::3]:6222"
  ]
}
```

Routes connect storage candidates to each other. Adding/removing routes
historically required restart; 2.14 may improve that — verify before
relying on it for routes (the documented reload support specifically calls
out leafnodes).

### Leafnodes

A leafnode is a NATS server that connects to another cluster as a client,
not as a peer. Subjects flow bidirectionally over the leafnode bridge,
but the leafnode is **not** part of the cluster's Raft consensus.

Two patterns:

1. **Leafnode without local JetStream** — bridge only. Every read crosses
   the leafnode to the hub.
2. **Leafnode with local JetStream in a separate domain** — bridge plus
   local durable storage. With mirrors of hub streams, reads are local.

Configured under `leafnodes {}`:

```
# on a hub storage candidate (server side)
leafnodes {
  listen: [fd00::1]:7422
}

# on a leaf machine (client side)
leafnodes {
  remotes: [
    { url: "nats://[fd00::1]:7422" }
  ]
}
```

**Runtime reload (2.14+)**: leafnode `remotes` can be updated without
restart. This is the primitive that makes dynamic peer reconfig
practical — adding a new hub member and pushing the new remote list to
all leaves no longer drops their connections.

### JetStream domains

A domain scopes JetStream metadata. Two domains == two independent Raft
clusters even within a single physical NATS deployment.

```
jetstream {
  domain: hub
  store_dir: /data/jetstream
}
```

The API prefix changes per domain: `$JS.API.>` becomes `$JS.<domain>.API.>`.
Mirrors and sources reference the domain via `external.api`.

For ployz the natural shape is one `hub` domain on the storage candidates,
plus per-machine leaf domains (`leaf-<machine_id>`) on mirror nodes. The
hub holds authoritative streams; leaf domains hold mirrors of them.

## Replication semantics

| R   | Quorum   | Survives                  | Notes                              |
|-----|----------|---------------------------|------------------------------------|
| 1   | 1 of 1   | nothing                   | single-node, no HA                 |
| 2   | 2 of 2   | nothing meaningful        | **never use** — same write avail as R=1, worse failure modes |
| 3   | 2 of 3   | any 1-node loss           | minimum HA                         |
| 5   | 3 of 5   | any 2-node loss           | high availability, higher write latency |

Quorum acks on the **fastest** quorum-sized subset, not all replicas. For
R=3 that means write latency is `max(RTT to fastest follower) + commit`,
which is one RTT, not two.

Reconfiguration: `stream update --replicas=N` is a Raft membership change.
Data copies from the leader to new replicas; copy time is proportional to
data size. During copy, writes still work — committed to the existing
quorum, asynchronously synced to new members. Once new replicas are
in-sync, quorum shifts to the new size.

Leader election after node loss: 250ms–2s typical, configurable.

## Coordination patterns

The four NATS-native patterns ployz builds on:

### KV CAS locks

Use `kv.create` to acquire (atomic, fails if held), `kv.update` with
revision precondition to renew, `kv.delete_expect_revision` to release.
TTL on each write (2.11+) bounds stale leases.

```
locks.deploy.<namespace>     pessimistic, held for the deploy duration
locks.cert.<hostname>        held only during ACME side effects
```

The lock's authority is **CAS write success**, never a stale `kv.get`.
Holders carry an opaque nonce; before mutating remote state, the executing
node re-checks lock + nonce.

### Work queues

A `Workqueue`-retention stream + a durable pull consumer with explicit
ack. Each message is delivered to exactly one worker; on ack the message
is removed; on `max_deliver` exceeded the message is dropped or DLQ'd.

```
stream cert_jobs (retention=Workqueue, ack=explicit, max_deliver=5)
  subjects: cert.jobs.renew.<hostname>
```

Combine with KV CAS locks for "exactly one worker, with a guard against
duplicate side effects":

```
worker pulls cert.jobs.renew.<hostname>
  → kv.create(locks.cert.<hostname>, …)  // bail with NACK if held
  → perform ACME, write cert KV, release lock
  → ack
```

### Scheduled messages

Publish with `Nats-Schedule`; broker emits at the scheduled time. Replaces
in-process schedulers for deterministic timed work. For ployz: schedule
cert renewal jobs at issuance time instead of running a renewal ticker.

### Request/reply RPC

Direct point-to-point RPC over subjects. Each daemon subscribes to
`node.<self_machine_id>.cmd.>` (queue group of size 1 to prevent local
fights). Callers send NATS requests with a built-in timeout; no-responder
detection fires immediately if the target is offline.

```rust
// async-nats sketch
let response = client
    .request_with_headers(
        format!("node.{target}.cmd.deploy.start_candidate"),
        headers_with_deploy_lock_nonce,
        payload,
    )
    .await?;
```

## ployz application

### Authority model

- KV — authoritative for independent single-key records. machines, invites,
  certificates metadata, acme_accounts, acme_challenges, instance_status.
- Stream — authoritative for ordered multi-record facts. `deploy_commits`
  (each `DeployCommit` envelope is one self-contained fact with revisions
  inlined). Append-only, immutable, no `MaxMsgsPerSubject` collapse.
- Local projection — read model, derived from the stream. Tail once,
  serve from cache.
- KV mirror — never as a correctness boundary. Available as a perf
  optimization later; rebuilt from the stream on every reader start.

`commit_deploy` publishes the envelope with `Nats-Expected-Last-Subject-Sequence: 0`
(create-only). Retries with the same `deploy_id` are idempotent.
`update_deploy_record` writes a separate `deploy_status` KV, never the
stream — keeps the commit log immutable and replay-safe.

### Replica policy

Replica count is operator intent, not a side effect of machine count.
Machine add only adds a machine and starts its local NATS role. It does not
promote the cluster to R=3, does not add a Raft voter to authoritative streams,
and does not change write quorum.

The allowed replica targets are:

| Target | Meaning |
|--------|---------|
| R=1 | single authoritative copy; no HA claim |
| R=3 | minimum HA; survives one storage candidate loss |
| R=5 | opt-in higher HA; survives two storage candidate losses |

R=2 is never selected. Reconfiguration is an explicit operator command
(`ployzctl nats storage promote ...`, name TBD), not a background reaction to
machine-add/remove. The command must produce a plan and fail loudly if the
requested storage set is not eligible.

Storage-promotion guardrails:

- exactly 1, 3, or 5 storage candidates for the requested replica target,
- every candidate is active, non-draining, and has a current local NATS health
  check,
- every candidate has persistent storage configured and enough free capacity for
  JetStream,
- every candidate is mutually reachable on NATS route and client ports over the
  overlay,
- candidate RTT and packet loss are inside the operator-selected latency class
  (`local`, `regional`, or explicitly accepted `cross-region`),
- candidates are spread across declared failure domains when region/AZ metadata
  exists,
- no candidate is already in an upgrade, remove, wipe, or bootstrap operation,
- demotion/removal plans preserve quorum unless the operator passes an explicit
  degradation flag and accepts the resulting R=1 state.

The command output should distinguish desired storage intent, current NATS
membership, stream replica status, catch-up progress, and any live observations
used to reject the plan.

### Topology roles

| Role               | nats-server config           | JetStream             | Purpose                                                          |
|--------------------|------------------------------|------------------------|------------------------------------------------------------------|
| `StorageCandidate` | `cluster { listen, routes }` | hub domain, R=N replicas | Authoritative storage. Raft voting member.                     |
| `Leaf`             | `leafnodes { remotes }`      | disabled or stub      | Bridge subjects to hub. Every read crosses the network.          |
| `Mirror` (planned) | `leafnodes { remotes }` + `jetstream { domain: leaf-<id> }` | leaf domain with mirrors of hub streams | Local read replica. Sub-ms reads. Read continuity through hub outage. |

The Mirror role is what we want as the eventual default for non-storage
machines. Today every machine is hardcoded as StorageCandidate (see
"Known gaps" below).

### Cluster lifecycle

| Transition  | What happens                                                                              |
|-------------|-------------------------------------------------------------------------------------------|
| 1 machine   | StorageCandidate, R=1. No fault tolerance.                                                |
| 1 → 2       | Joiner is added as Leaf/Mirror/eligible candidate according to the invite. Replicas stay R=1. |
| 2 → 3       | Joiner is added. Replicas still stay R=1 until an explicit storage-promotion command succeeds. |
| explicit R=3 promotion | Operator selects three eligible storage candidates; NATS assets reconfigure to R=3 after plan validation. |
| 3 → 2 planned   | Demote first (drops to R=1 with `--accept-storage-degradation`), then remove the Leaf. |
| 3 → 2 unplanned | R=3 maintains quorum at 2/3. Leader election ~250ms–2s. Recovery on rejoin.        |
| 2 simul. losses on R=3 | Below quorum. Reads stale, writes blocked. Data plane keeps serving cache. |

### Latency budget (WireGuard 1–15ms intra-region, 30–80ms cross-region)

| Operation                     | 1-node | 2-node leaf  | R=3 intra-region | R=3 cross-region |
|-------------------------------|--------|--------------|------------------|------------------|
| KV get on local leader        | <1ms   | <1ms         | <1ms             | <1ms             |
| KV get on non-leader          | —      | 2–4ms        | 6–16ms           | 31–81ms          |
| KV put                        | <1ms   | 2–4ms        | 6–30ms           | 31–160ms         |
| Stream publish                | <1ms   | leaf RTT+2ms | 6–30ms           | 31–160ms         |
| Watcher event delivery        | local  | leaf RTT     | 1 hop            | 1 hop            |
| Leader re-election            | n/a    | n/a          | 250ms–2s         | 500ms–3s         |

### Schema overview

Streams:

- `deploy_commits` — append-only `DeployCommit` envelopes.
  Subject `deploy_commits.<ns>.<deploy_id>`. Retention=Limits, no
  `max_age`, no `max_messages_per_subject` collapse. R per policy.
- `revisions` — audit-only history of `ServiceRevisionRecord`. Subject
  `revisions.<ns>.<svc>.<hash>`, `MaxMsgsPerSubject=1`. Not on the
  projection critical path (revisions are inlined in commits).
- `cert_jobs` — Workqueue retention. Subjects under `cert.jobs.renew.<hostname>`,
  `cert.jobs.finalize.<order_id>`.

KV buckets:

- `machines` — key=machine_id.
- `invites` — key=invite_id.
- `instances` — key=instance_id. No TTL. Removed only by explicit event.
- `acme_accounts` — key=hash(issuer_url).
- `certificates` — key=hostname (metadata only). PEM material in Object
  Store.
- `acme_challenges` — key=`<hostname>.<token>`.
- `deploy_status` — key=deploy_id. Mutable lifecycle overlay over the
  immutable commit envelope.
- `locks` — key=`locks.deploy.<ns>` / `locks.cert.<hostname>`. TTL leases.
- `coordinator_lease` — key=election lease id. TTL leases.

### Operational details

- **Ports.** Client `4222`, route `6222`, leafnode `7422` — all on overlay
  V6 IP. Monitoring `8222` — `127.0.0.1` only. The monitoring port has no
  auth in NATS by design. ZFS transfer payloads use the separate daemon
  `zfs_transfer_port` setting, default `4319`; that listener is a data-plane
  byte-stream endpoint, not a control-plane command port.
- **Health probe.** Daemon's primary health is its own async-nats
  round-trip on `4222` (works in Host and Docker because the client port
  binds to overlay). `/healthz` on `8222` is reached via `docker exec`
  for deep diagnostics.
- **Security perimeter.** WireGuard. Anything routable on the V6 mesh is
  treated as authenticated.

### Known gaps (status of the implementation)

Tracked in code comments and the implementation plan. Highlights:

1. Routing events are live JetStream atomic batches. Gateway/DNS use durable
   per-machine consumers; runtime watches and readiness probes use temporary
   consumers that are deleted when the watcher closes. Routing subscription
   setup replaces any old consumer with the same id so a fresh snapshot is the
   catch-up boundary. Updates carry either a complete batch or an explicit
   consumer failure.
   Routing consumer `max_ack_pending` is bounded to the local bridge-channel
   capacity, and idle heartbeats surface broken delivery paths as failures.
2. Machine/certificate/ACME challenge subscriptions are KV watchers that carry
   either a domain event or an explicit watcher failure. If a watcher fails or
   closes, consumers stop using the stale event stream. Mesh task groups cancel
   on unexpected task exit; broader operator status remains a hardening item.
   KV subscriptions create the watch before loading the initial snapshot, so
   updates after the watch boundary are delivered even if they race with the
   snapshot read.
3. `replay_projection` runs full N-RPC replay on every read.
4. Lock TTL doesn't refresh on `kv.update` because the current async-nats
   API doesn't expose update-with-TTL — workaround is a delete+create
   cycle (race risk) or wait for the client API to catch up to 2.11+
   server features.
5. Standalone gateway and DNS now use NATS-backed store subscriptions through
   their `main.rs` wrappers.
6. Coordination layer — KV lock helpers and node RPC are now the daemon command
   path. `PendingReservations`, `OverlayIssuanceCoordinator`, the peer TCP RPC
   client, and the peer control listener have been removed. Remaining direct TCP
   is narrow data movement such as ZFS send/receive payload streams.
7. `MachineRole` is hardcoded to `StorageCandidate` in join/bootstrap flows.
8. Joiner bootstrap still uses SSH stdio to deliver the bootstrap command and
   scoped cluster information. The introducer writes a bootstrap membership seed
   into NATS so existing nodes learn the joiner's WireGuard identity through the
   machines subscription; after bootstrap, node commands use NATS request/reply.
   The intended v2 flow is for the joiner to receive scoped NATS credentials and
   publish its own membership without an introducer-authored seed.
9. `subjects::subject_token` collision risk for namespace/hostname names
   that differ only in punctuation.

## Future planning

### Long-term target

Ployz's control-plane target is documented in
[`docs/nats-native-control-plane.md`](nats-native-control-plane.md). The short
version: NATS is the native authority for state, coordination, node commands,
membership changes, and failure visibility. Machine add does not promote storage
authority; R=3/R=5 promotion is an explicit guarded operator operation. TCP
remains only for true byte streams such as ZFS send/receive payloads.

### Mirror role default

The biggest open architectural question is making Mirror the default for
non-storage machines. Today's hardcoded-StorageCandidate pattern means
adding any machine grows the Raft cluster, which is wrong for mesh sizes
beyond 3–5.

Concrete shape:

- Add `MachineRole::Mirror` variant alongside `StorageCandidate` and
  `Leaf`.
- Default new-joiner role: Mirror/Leaf. Becoming an authoritative storage
  candidate is operator intent recorded by a storage-promotion command, not a
  consequence of being the first, second, or third machine.
- Mirror config templating: `leafnodes.remotes` plus
  `jetstream { domain: leaf-<machine_id> }`, plus mirror provisioning for
  each hub stream/KV the local services need.
- ployzd on a Mirror machine connects to its local NATS; reads come from
  local mirrors, writes route through the leafnode bridge.
- Mirror provisioning runs on explicit role/intent changes. Machine add writes
  membership and connectivity state; it does not silently reconfigure
  authoritative stream replicas.

The plan section of `docs/future/` should grow a phase for this once the
storage layer's known gaps are closed.

### Zero-trust auth (NKeys + JWT)

Currently the WireGuard overlay is the trust boundary. That's a coarse
model — a compromised machine has full read/write access to all cluster
state.

NATS supports operator/account/user JWT chains with NKey-based identities
and per-subject permissions. The migration would be:

- Issue a per-machine NKey at machine-add.
- Encode an account JWT into the invite token.
- Configure nats-server with `accounts {}` and per-user JWT auth.
- Map domain capabilities ("can deploy to namespace X", "can renew cert
  for hostname Y") to NATS subject permissions where possible. Domain
  capabilities NATS can't express (e.g. namespace-scoped deploy authority)
  stay as in-process authorization checks.

This is its own scoped piece of work. The substrate is ready for it; the
identity issuance and permission policy are not. Worth doing once we want
to support multi-operator clusters or treat node compromise as a recoverable
event.

### Compaction with checkpoint

`deploy_commits` is unpruned for v1 because pruning a transition log
loses correctness — old commits represent active releases. The forward
plan when history grows large enough to matter:

- Periodic per-namespace snapshot blob written to Object Store. Contains
  the full effective state (releases, volumes) at a known stream sequence.
- Single `checkpoint.<namespace>.<seq>` event published to a separate
  stream that readers gate on.
- Once the checkpoint is durable, `delete_message` on `deploy_commits`
  for sequences ≤ checkpoint is safe.
- Readers replay from the latest checkpoint's sequence forward.

This is idempotent NATS work — Object Store and stream subject deletion
are both first-class. Comes off the shelf when needed.

### Multi-region

Cross-region R=3 has 30–160ms write latency. Single-region clusters with
async cross-region mirrors are usually the better shape.

Sketch:

- Region A: 3 storage candidates, hub domain, full R=3.
- Region B: 3 storage candidates in a separate cluster + JetStream
  domain, R=3 within Region B.
- Cross-region: each region's `deploy_commits` mirrors the other's. Reads
  are local; writes are local-first, replicated async to the other region.
- Failover: if Region A is lost, promote Region B's mirrors to primary
  (2.12+ feature). Writes resume in Region B.

This is more about ployz's product story than NATS — NATS handles
multi-region cleanly with mirrors and domain APIs. The hard part is
deciding which region "owns" a workload at any given time.

### Scheduled cert renewal

Direct application of 2.14's `Nats-Schedule` header. Replace the
leader-elected ticker:

```
on cert issued/renewed:
  publish to cert.jobs.renew.<hostname>
    with Nats-Schedule = <renewal_due_time>
    with Nats-Msg-Id = "renew-<hostname>-<not_after>"
```

NATS holds the message until the scheduled time, then delivers it to the
work queue. One worker pulls, performs ACME, schedules the next renewal
on success. The ticker disappears entirely.

### Consumer reset for projection migrations

When `DeployCommit` gains or changes a field and the in-memory projection
logic changes, every reader needs to rebuild its projection from sequence
0. Today this means deleting and recreating the durable consumer. With
2.14's reset API it's one call — preserves the consumer name and any
attached subscribers.

Worth wiring into a `ployzctl admin nats reset-consumers` operator
command for deliberate version bumps.

## References

- [NATS 2.11 release notes](https://docs.nats.io/release-notes/whats_new/whats_new_211)
- [NATS Server 2.11 release blog](https://nats.io/blog/nats-server-2.11-release/)
- [NATS 2.12 release notes](https://docs.nats.io/release-notes/whats_new/whats_new_212)
- [NATS Server 2.12 release blog](https://nats.io/blog/nats-server-2.12-release/)
- [NATS Server releases](https://github.com/nats-io/nats-server/releases)
- [JetStream concepts](https://docs.nats.io/nats-concepts/jetstream)
- [Source and Mirror streams](https://docs.nats.io/nats-concepts/jetstream/source_and_mirror)
- [JetStream on leaf nodes](https://docs.nats.io/running-a-nats-service/configuration/leafnodes/jetstream_leafnodes)
- [Key-Value Store concepts](https://docs.nats.io/nats-concepts/jetstream/key-value-store)
- [JetStream wire API reference](https://docs.nats.io/reference/reference-protocols/nats_api_reference)
- [async-nats Rust client releases](https://github.com/nats-io/nats.rs/releases)
