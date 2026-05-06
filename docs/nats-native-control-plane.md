# NATS-Native Control Plane Target

This is the long-term target for making ployz NATS-native. It is intentionally
about product and failure semantics, not a porting checklist.

For the long-term subject, authority, route-export, and regional topology shape,
read [`nats_future.md`](nats_future.md).

## Target

Ployz uses NATS as its native control plane. Durable state, coordination, node
commands, membership changes, failure visibility, and operator workflows are
designed around NATS primitives first:

- JetStream streams for ordered immutable facts.
- KV CAS for independent records and leases.
- Request/reply for bounded node commands.
- Work queues for exactly-one background work.
- Scheduled messages for broker-owned timers.
- Sources for intentional projections, and mirrors for read locality,
  migration, and disaster recovery.

Direct TCP remains only for true byte streams such as ZFS send/receive payloads.

The product topology separates regions from authorities:

- **installation** — compute, trust, and substrate boundary. Separate compute
  pools usually mean separate installations.
- **namespace** — deploy/environment boundary inside an installation. Prod,
  staging, preview, and PR environments are normally namespaces.
- **region** — placement, latency, route serving, and machine grouping,
- **authority** — durable write ownership, quorum, and failure domain,
- **home/data region** — the region hosting an authority's HA JetStream state,
- **compute-only region** — a region that can run workloads and gateways but
  does not own independent durable control-plane state yet.

For the MVP, ployz can have many regions but one HA home/data authority. Global
deploys are still allowed; they fan out regional execution while committing
durable truth to the owning authority. A later regional data authority is created
only by an explicit promotion operation.

## Non-Negotiables

- Machine add does not change storage authority.
- Machine add in a new region does not create a new data authority unless the
  operator explicitly promotes that region.
- Promotion to R=3 or R=5 is an explicit operator operation.
- Regional data-authority promotion is an explicit operator operation.
- No background loop silently changes quorum, placement, or operator intent.
- Every mutation has a foreground caller or an operator-visible failure surface.
- The data plane keeps serving last good runtime state when control-plane writes
  are unavailable.
- Any split-brain risk is resolved by refusing writes, not by automatic failover.

## Operator Workflows

### Machine Add

`machine add` admits a new member, records its region, and establishes
connectivity. It starts the local NATS server according to node capabilities and
defaults to `storage=true`, but it does not:

- increase JetStream replica count,
- add a Raft voter to authoritative streams,
- change write quorum,
- rebalance storage,
- promote the node or its region to storage authority.

The operation succeeds when the machine has published its membership, local NATS
connectivity is observable, and the operator can see its eligibility, capacity,
and region. If the region did not exist, creating the region record is substrate
metadata only; durable control-plane state stays in the current home/data
authority.

### Storage Promotion

Storage promotion is a separate command, conceptually:

```text
ployzctl nats storage plan --replicas 3 --candidates a,b,c
ployzctl nats storage promote --replicas 3 --candidates a,b,c
```

The plan must show:

- selected candidates,
- current replica state per stream/KV bucket,
- route reachability and RTT/loss,
- storage capacity and persistence,
- failure-domain spread,
- estimated catch-up work,
- exact degradation risk during the change,
- rollback/demotion instructions.

Promotion succeeds only after NATS reports the assets at the requested replica
count and candidates are caught up. Partial promotion is a failed operation with
explicit status.

`ployzctl status` reports each authoritative stream/KV-backed asset with the
configured replica count, current replica count, offline replica count, maximum
reported follower lag, and leader when NATS exposes one. A replica-count match
alone is not enough to call promotion healthy; the status row must show the
requested replicas current, zero offline replicas, and zero lag.

### Regional Data Promotion

A compute-only region becomes a data authority only through an explicit
foreground operation, conceptually:

```text
ployzctl region promote sin --data --replicas 3
```

The plan must show:

- selected storage-enabled machines in that region,
- current home/data authority and the state selected for transfer or
  initialization,
- NATS account/domain changes,
- stream/KV/Object Store assets to create or reconfigure,
- route export/projection changes,
- expected write downtime or quiescence if ownership moves,
- rollback/demotion instructions.

Promotion succeeds only after the new regional authority/domain exists, selected
assets are healthy at the requested replica count, and any route projections
needed for serving are observable. It does not automatically move all workloads,
volumes, streams, or namespaces. Those remain explicit placement, fork, migrate,
or promote operations.

### Storage Demotion And Removal

Removing a machine that participates in storage authority requires an explicit
demotion plan first. The demotion must preserve quorum unless the operator passes
an explicit degradation flag and accepts the resulting R=1 state.

Machine removal removes membership intent only after storage authority, routing,
and workload placement have been addressed by explicit commands.

### Upgrade

Upgrade is an operator-directed rolling operation:

1. Pick one node.
2. Check NATS health, stream replica state, and data-plane continuity.
3. Mark the node in upgrade intent/status.
4. Upgrade and restart.
5. Wait for NATS reconnect/catch-up and daemon health.
6. Move to the next node.

The system should never infer that a restarted upgraded node is safe merely
because it reappeared. The command verifies it.

## Storage Promotion Guardrails

Promotion to R=3 or R=5 must fail before mutation unless all guardrails pass:

- selected storage-enabled node count exactly matches the requested replica count,
- every selected node is active and non-draining,
- every selected node has persistent JetStream storage configured,
- free capacity is sufficient for current data plus catch-up margin,
- client and route ports are reachable over the overlay,
- NATS health check succeeds locally and remotely,
- route RTT/loss fit the selected latency class,
- selected nodes are not in bootstrap, remove, wipe, or upgrade operations,
- selected nodes are spread across declared region/AZ/failure domains,
- current stream/KV assets can reconfigure without crossing below quorum,
- operator has acknowledged any planned degradation.

Guardrail failures are useful output. They should tell the operator exactly what
must change before retrying.

## Failure Analysis

| Situation | Expected behavior | Operator surface |
|-----------|-------------------|------------------|
| Single node, healthy | R=1 reads/writes work. No HA claim. | Status shows single-copy storage. |
| Single node, daemon restart | NATS/data plane keep running or are adopted. | Restart reports adopted services. |
| Single node, node down | Whole control plane unavailable. | No false HA messaging. |
| Add second node | Member joins. Replicas remain R=1. | Status shows non-authoritative node. |
| Add third node | Member joins. Replicas remain R=1. | Status suggests eligible R=3 plan, not automatic promotion. |
| Explicit R=3 promotion | Assets reconfigure after guardrails pass. | Plan, progress, caught-up status. |
| One R=3 storage node offline | Writes continue after leader election. | Degraded-but-writable status. |
| Two R=3 storage nodes offline | Writes fail loudly below quorum. | Mutations blocked; data plane last-good. |
| Offline leaf/mirror | Commands to that node no-responder/timeout. | Foreground operation fails for that target. |
| Offline node rejoins | It catches up from NATS; no intent rewrite. | Rejoin/catch-up observation. |
| Planned storage removal | Demote first, then remove. | Quorum-preserving plan required. |
| Unplanned storage loss | Stored intent remains; status marks loss. | Operator chooses replace, demote, or wait. |
| Network partition with quorum side | Quorum side accepts writes; minority cannot. | Minority reports below quorum/unavailable. |
| Cross-region latency | Writes pay quorum RTT. | Plan warns or requires cross-region acknowledgement. |
| Compute-only region loss | Home/data authority remains writable; replicas/routes in that region go unavailable. | Regional placement and route freshness show degraded. |
| Home/data region loss | Durable writes unavailable unless another authority was explicitly prepared/promoted. Last-good data plane may keep serving where it still has routes/backends. | Operator restores home region, promotes prepared DR, or accepts data-loss recovery. |

## Regional Shape

Default regional guidance:

- Keep regions in the model from day one.
- Keep one HA home/data authority for the MVP.
- Let compute-only regions run workloads, gateways, and regional route serving
  without owning durable control-plane writes.
- Avoid cross-region quorum unless the operator explicitly accepts latency and
  failure-mode tradeoffs.
- Use sources for public/shared route projections.
- Use mirrors for read locality, migration, and disaster recovery, not as the
  normal way to make regions equal.
- Promote a region into a data authority only through an explicit foreground
  operation.

The hard product question is ownership. NATS can route, source, mirror, and
promote; ployz must decide which authority owns writes for a workload, stream,
volume, or namespace at any moment, and that ownership change must be an
explicit operation.

## Latency Semantics

Latency should be documented by internal data path, because NATS-native does not
mean every read or write touches every node.

Planning assumptions:

- same host / local NATS: `<1ms`
- same LAN / same rack overlay: `1-5ms RTT`
- same region over WireGuard: `5-20ms RTT`
- nearby regions: `25-60ms RTT`
- distant regions: `80-180ms RTT`

These are planning ranges, not product SLOs. E2E tests should measure them with
fault injection and record observed p50/p95/p99.

### Where Data Lives

| Data | Authority | Physical location | Read path | Write path |
|------|-----------|-------------------|-----------|------------|
| Region registry | NATS KV `regions` | installation-root/home authority replicas | local daemon projection or KV direct get | KV CAS/put to home authority quorum |
| Machine membership | NATS KV `machines` | installation-root/home authority replicas | local daemon projection or KV direct get | KV CAS/put to home authority quorum |
| Invites | NATS KV `invites` | home/data authority replicas | direct KV | KV create/update to home authority quorum |
| Deploy commits | JetStream stream `deploy_commits` | owning authority replicas | daemon projection from stream | append one immutable commit to owning authority quorum |
| Deploy status | NATS KV `deploy_status` | owning authority replicas | direct KV/projection | mutable KV update to owning authority quorum |
| Instance status | NATS KV `instances` | owning authority replicas | routing projection or direct KV | participant writes status to owning authority quorum |
| Routing snapshot | local projection | each gateway/DNS/daemon process memory | in-process memory | rebuilt from authoritative NATS state |
| Routing events | JetStream stream `routing_events` | owning authority replicas | durable or temporary consumer | atomic batch publish to owning authority quorum |
| Public/shared route projections | JetStream source streams | installation-root projection authority | public/shared gateways consume projections | source only from explicit owner exports |
| Certificates metadata | NATS KV `certificates` | owning authority replicas | direct KV/subscription | KV put to owning authority quorum |
| Certificate PEM blobs | NATS Object Store | owning authority replicas | object get, often cached by consumers | object put to owning authority quorum |
| ACME challenges | NATS KV `acme_challenges` | owning authority replicas | gateway/cert reader projection | KV put/delete to owning authority quorum |
| Locks/leases | NATS KV `locks` | owning authority replicas | direct KV only for diagnostics | CAS create/update/delete to owning authority quorum |
| Cert jobs | JetStream work queue | owning authority replicas | worker pull consumer | publish to owning authority quorum, ack to owning authority quorum |
| Scheduled renewals | JetStream scheduled message | broker-owned in owning authority | delivered to work queue at due time | scheduled publish to owning authority quorum |
| Node commands | core NATS request/reply | not durable; target daemon subscription | request travels to target daemon | reply from target daemon |
| ZFS datasets/snapshots | local node disk | node that owns the volume | local zfs command or node RPC metadata | local zfs command; transfer bytes over TCP |
| Mirror read copies | mirror streams/KV in another domain | mirror node/region local disk | local mirror read | async replication from owner; not write authority |

The important split:

- Durable service projections use stable per-machine consumers.
- Short-lived watches use ephemeral memory-backed consumers with an inactivity
  threshold and do not leave durable cursor state behind if the watcher exits or
  the daemon crashes.
- Routing batch subscriptions carry complete atomic batches or explicit
  consumer failures, so projection consumers can reload instead of silently
  continuing from a stale event stream. Durable subscription setup replaces any
  old consumer with the same id after reading the routing stream sequence and
  loading a fresh snapshot; the new consumer starts from the next stream
  sequence. Routing consumers bound in-flight delivery to the local bridge
  channel and use idle heartbeats to detect broken delivery.
- KV watcher consumers must treat watcher failure or closure as a lost
  freshness boundary. Machine, certificate, and ACME challenge subscriptions
  carry explicit failure updates; consumers should stop using the stale stream,
  reload from authority, or surface degraded health to an operator-visible
  status. KV subscriptions read the bucket stream sequence as the snapshot
  boundary, load the current snapshot, then watch from the next sequence so
  updates that race with snapshot loading are still delivered. KV watcher tasks
  terminate when their downstream receiver closes, and malformed event payloads
  become explicit subscription failures because stale projections need an
  operator-visible audience.
- Edge projections expose their subscription freshness as sidecar metrics:
  gateway reports routing, certificates, and ACME challenge streams; DNS reports
  routing. When those metrics endpoints are configured, `ployzctl status`
  includes an `edge_sync` row for each stream so an operator can distinguish
  stale routing projection from a healthy data-plane process. The metrics also
  expose when the current health state began and a cumulative failure count, so
  stale projections have age and trend signals instead of a bare boolean.
  Gateway routing, certificate, and ACME subscriptions are one snapshot
  generation: if any stream setup or delivery fails, the generation is dropped
  and all three streams are marked stale until a fresh snapshot and all
  subscriptions are established again.
- The daemon's NATS node RPC listener records its own subscription freshness in
  `nats-node-rpc-health.json` under the network data directory. `ployzctl
  status` reports that as `control_plane component=node_rpc_listener`, including
  stale-since time, consecutive failures, and the last listener error.
  Subscription loss, resubscribe failures, and command-path failures such as
  daemon channel closure or response publish failure all land in that health
  row. While the listener is stale, foreground request/reply calls to that node
  fail with no-responder or timeout; recovery is the listener resubscribing or a
  later successful command clearing stale command-path health, not a hidden
  control-plane fallback.
- The daemon's certificate renewal worker records work-queue health in
  `nats-cert-renewal-health.json` under the network data directory. `ployzctl
  status` reports it as `control_plane component=cert_renewal_worker`, including
  stale-since time, consecutive failures, and the last fetch, job, ack, or nak
  error. A fetch failure can clear after the consumer fetch path recovers; a job
  failure remains stale until a renewal job completes and acks successfully.
- The daemon's bootstrap seed-cache task records whether its machines
  subscription is fresh in `bootstrap-seed-cache-health.json` under the network
  data directory. `ployzctl status` reports it as
  `control_plane component=bootstrap_seed_cache`; stale health means the local
  `bootstrap-peers.json` restart hint may lag NATS membership authority.
- Mesh background tasks that consume NATS-backed machine subscriptions are also
  reported under `control_plane` with `mesh_*` component names. If a machine
  subscription closes or reports a watcher failure, the task exits, the mesh task
  set cancels, and status preserves which local projection task went stale.

- **authority** lives in the owning authority's JetStream/KV/Object Store,
- **hot read models** live in process memory and are rebuilt from authority,
- **node-local reality** lives on the node and is queried by request/reply,
- **payload bytes** live in the substrate that owns them, such as ZFS.

### Internal Operation Classes

| Operation class | NATS path | Latency driver | Semantics |
|-----------------|-----------|----------------|-----------|
| Local read from in-memory projection | daemon memory | none after projection is warm | fastest path; may be last-good when NATS is unavailable |
| Local KV direct get on local leader | JetStream/KV | local broker and disk/cache | sub-ms to low-ms in healthy local setups |
| KV/stream write at R=1 | one NATS server | local broker fsync/config | available only while that node is up |
| KV/stream write at R=3 same region | Raft quorum | fastest follower RTT + broker work | one follower can be slow/offline without blocking after election |
| KV/stream write at R=3 cross-region | Raft quorum across WAN | WAN RTT to fastest quorum | high tail latency; should require explicit operator acceptance |
| NATS node request/reply same region | core NATS route/leaf path | round trip to target daemon | no durable write unless command performs one |
| NATS placement probe cross-region | core NATS request/reply | round trip to candidate machines | live capacity signal only; no placement intent until deploy commits |
| NATS request/reply to offline node | core NATS no responder or timeout | subscription absence or timeout | foreground failure; caller decides retry/abort |
| NATS node listener subscription loss | daemon edge task | local NATS client reconnect/resubscribe | daemon resubscribes; callers see no responder/timeout while absent; local status records listener staleness |
| Work queue dispatch | JetStream write + consumer delivery | stream write plus consumer pull/ack | exactly-one-worker behavior, not zero-latency signaling |
| Scheduled message | broker schedule | schedule time plus dispatch latency | daemon restart does not lose timer |
| Mirror read | local mirror stream/KV | mirror lag and local read | fast but not authoritative for writes |
| ZFS transfer | direct TCP byte stream | bandwidth, RTT, disk, compression | control may be NATS; payload is streaming TCP |

### Internal Flows

#### Deploy apply

Data touched:

1. `locks.deploy.<namespace>` KV CAS acquire.
2. Current deployment/routing state read from local projection or owning
   authority.
3. Participant runtime commands over NATS request/reply.
4. Participant writes `instances` status records as candidates become ready.
5. One append to `deploy_commits`.
6. `deploy_status` KV update.
7. Gateway/DNS projections reload from the commit/status state.

Latency shape:

- lock acquire: one owning-authority quorum write,
- placement probes: request/reply RTT to candidate regions/machines,
- each participant command: request/reply RTT to that node plus runtime work,
- readiness: dominated by container startup/probe time,
- commit: one owning-authority quorum stream append,
- route visibility: watcher/projection delivery plus local rebuild.

For small same-region clusters, the NATS portions should be low-ms to tens of
ms. Container startup and readiness dominate deploy time.

#### Machine add

Data touched:

1. invite read/update in `invites`,
2. introducer writes a bootstrap membership seed to `machines` so existing
   nodes learn the joiner's WireGuard identity through NATS,
3. joiner local NATS startup/connectivity,
4. joiner overwrites its `machines` membership record from its own daemon,
5. existing daemons observe membership through subscription/projection.

Latency shape:

- bootstrap/install is out-of-band and dominates,
- bootstrap and self-published membership writes are installation-root/home
  authority quorum writes,
- visibility is subscription delivery plus local projection,
- no stream/KV replica reconfiguration occurs.

#### Storage promotion

Data touched:

1. live NATS health/probe observations from each candidate,
2. stream/KV/Object Store metadata for all authoritative assets,
3. reconfiguration of each asset to the requested replica count,
4. catch-up state until new replicas are current,
5. durable storage intent/status record once the plan is committed.

Latency shape:

- plan: mostly request/reply probes and metadata reads,
- apply: one reconfiguration per asset plus data copy,
- total time is proportional to data size and slowest accepted candidate,
- the operation is not tied to machine-add latency.

#### Machine remove

Data touched:

1. live status/probes for target and affected peers,
2. workload placement/routing state if workloads must move,
3. storage demotion/reconfiguration if the target is authoritative,
4. final `machines` membership update/delete.

Latency shape:

- non-storage removal is mostly workload/runtime work plus one membership write,
- storage removal includes catch-up/reconfiguration latency,
- if demotion would violate quorum, the operation fails before membership change.

#### Cert renewal

Data touched:

1. `cert_jobs` work queue message,
2. `locks.cert.<hostname>` KV CAS lease,
3. `acme_challenges` KV records,
4. external ACME service,
5. `certificates` KV metadata,
6. Object Store PEM blob.

Latency shape:

- NATS coordination is a scheduled work queue delivery plus a few
  owning-authority writes,
- external ACME and HTTP-01 validation dominate,
- scheduled renewal timing is broker-owned and survives daemon restart.

#### ZFS transfer

Data touched:

1. source node local ZFS snapshot metadata,
2. destination node local ZFS receive state,
3. NATS request/reply for setup/metadata commands,
4. direct TCP stream for send payload,
5. final volume/routing metadata write if ownership changes.

The direct listener is configured by `zfs_transfer_port` and defaults to
`4319`. It is intentionally named as a transfer endpoint: no daemon command
authority is attached to this port.

Latency shape:

- setup is request/reply RTT plus local zfs command time,
- payload transfer is bandwidth/disk/RTT dependent,
- final metadata publish is one owning-authority quorum write.

### Common Operator Operations

| Operator operation | Expected latency shape | Notes |
|--------------------|------------------------|-------|
| `status` local | local projection + health probes | should be fast; mark freshness explicitly if projection is stale |
| `status --live` cluster | NATS request/reply fan-out or sampled probes | bounded by slow/offline targets unless output is per-node partial |
| `deploy preview` | reads + live reachability probes | should not write; fails if required live facts cannot be checked |
| `deploy apply` | deploy lock KV write + participant commands + commit stream write | lock/commit pay quorum; runtime start/readiness dominates total time |
| `machine add` | bootstrap out-of-band + membership write | does not pay storage-promotion cost |
| `machine add --region <new>` | bootstrap out-of-band + region/membership writes | creates substrate region metadata, not a data authority |
| `storage promote R=3 plan` | live probes + NATS metadata reads | read/probe-heavy; no mutation until apply |
| `storage promote R=3 apply` | stream/KV reconfiguration and catch-up | proportional to data size and slowest selected candidate catch-up |
| `region promote --data` | live probes + account/domain setup + selected state transfer/init | explicit future operation; does not move all state by default |
| `machine remove non-storage` | membership/workload operations | no quorum demotion unless the node owns storage authority |
| `machine remove storage` | demotion plan + replica reconfiguration + membership update | must preserve quorum or require explicit degradation |
| `cert renew` | work queue delivery + ACME network + KV writes | ACME dominates; lock/write latency should be small |
| `rolling upgrade step` | node command + restart + NATS catch-up checks | bounded by service restart and catch-up, not background guessing |

### R=3 Write Latency

For R=3, a write commits when the leader and one follower have the entry. That
means the write latency is roughly:

```text
max(RTT to fastest healthy follower, local disk/broker work) + client path
```

It is not the sum of all three nodes. A slow third node affects catch-up and
degraded status, not the happy-path write, as long as quorum remains healthy.

### Storage-Eligible Nodes

Nodes do not have permanent storage or mirror identities. A node can be
`storage=true` and currently host no stream replicas, or host replicas for some
streams and app workloads for others. The control plane should report stored
eligibility separately from current stream placement and live observations.

### Offline And Slow Nodes

Offline nodes should be cheap to understand:

- request/reply with no subscriber returns no responders when NATS can know that,
- otherwise the caller hits a configured timeout,
- the operation reports the specific target that did not answer,
- unrelated storage writes continue if quorum is healthy.

Slow nodes are different from offline nodes. Slow nodes may still be eligible for
some roles, but storage promotion guardrails should reject them for a latency
class unless the operator explicitly accepts that class.

### What E2E Should Measure

The e2e suite should record latency observations for:

- local R=1 KV write,
- same-region R=3 KV write,
- same-region NATS request/reply,
- offline-node no-responder/timeout,
- R=3 one-node-loss write after election,
- below-quorum failed mutation,
- mirror lag under normal load,
- cross-region quorum write with explicit acceptance,
- global deploy placement probes to remote regions,
- ZFS transfer setup latency separately from transfer throughput.

## E2E Target

E2E scenarios should prove product primitives and failure guarantees:

- `single_node_bootstrap_r1`
- `machine_add_does_not_promote_storage`
- `storage_promote_r3_rejects_unhealthy_candidate`
- `storage_promote_r3_rejects_same_failure_domain`
- `storage_promote_r3_rejects_high_latency_without_ack`
- `storage_promote_r3_success_reports_catchup`
- `r3_one_storage_node_offline_remains_writable`
- `r3_below_quorum_blocks_mutations`
- `offline_leaf_node_command_fails_loudly`
- `storage_node_rejoin_catches_up_without_intent_rewrite`
- `planned_storage_removal_requires_demote`
- `rolling_upgrade_checks_quorum_between_nodes`
- `global_deploy_uses_region_capacity_offers`
- `compute_region_loss_preserves_home_authority`
- `regional_authority_promotion_requires_operator`
- `regional_projection_read_local_write_owner_explicit`
- `home_region_failover_requires_operator_promotion`

Tests should assert NATS-visible behavior where that is the product contract:
stream replica count, KV/stream write failure, request/reply no responders,
consumer catch-up, projection/mirror lag, and operator-visible status. Direct
TCP assertions belong only to bulk transfer scenarios.
