# NATS-Native Control Plane Target

This is the long-term target for making ployz NATS-native. It is intentionally
about product and failure semantics, not a porting checklist.

## Target

Ployz uses NATS as its native control plane. Durable state, coordination, node
commands, membership changes, failure visibility, and operator workflows are
designed around NATS primitives first:

- JetStream streams for ordered immutable facts.
- KV CAS for independent records and leases.
- Request/reply for bounded node commands.
- Work queues for exactly-one background work.
- Scheduled messages for broker-owned timers.
- Mirrors/sources for read locality and disaster recovery.

Direct TCP remains only for true byte streams such as ZFS send/receive payloads.

## Non-Negotiables

- Machine add does not change storage authority.
- Promotion to R=3 or R=5 is an explicit operator operation.
- No background loop silently changes quorum, placement, or operator intent.
- Every mutation has a foreground caller or an operator-visible failure surface.
- The data plane keeps serving last good runtime state when control-plane writes
  are unavailable.
- Any split-brain risk is resolved by refusing writes, not by automatic failover.

## Operator Workflows

### Machine Add

`machine add` admits a new member and establishes connectivity. It may start a
local NATS server as Leaf/Mirror/eligible storage candidate according to the
invite and node capabilities, but it does not:

- increase JetStream replica count,
- add a Raft voter to authoritative streams,
- change write quorum,
- rebalance storage,
- promote the node to storage authority.

The operation succeeds when the machine has published its membership, local NATS
connectivity is observable, and the operator can see its eligible roles.

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

- candidate count exactly matches the requested replica count,
- every candidate is active and non-draining,
- every candidate has persistent JetStream storage configured,
- free capacity is sufficient for current data plus catch-up margin,
- client and route ports are reachable over the overlay,
- NATS health check succeeds locally and remotely,
- route RTT/loss fit the selected latency class,
- candidates are not in bootstrap, remove, wipe, or upgrade operations,
- candidates are spread across declared region/AZ/failure domains,
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
| Region loss | No automatic opposite-region takeover. | Operator promotes mirror/failover explicitly. |

## Regional Shape

Default regional guidance:

- Prefer one regional R=3 hub for authoritative writes.
- Use mirrors for remote read locality and disaster recovery.
- Avoid cross-region quorum unless the operator explicitly accepts latency and
  failure-mode tradeoffs.
- Treat mirror promotion as disaster recovery, not load balancing.

The hard product question is workload ownership. NATS can mirror and promote;
ployz must decide which region owns writes for a workload at any moment, and that
ownership change should be an explicit operation.

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
| Machine membership | NATS KV `machines` | authoritative hub JetStream replicas | local daemon projection or KV direct get | KV CAS/put to hub quorum |
| Invites | NATS KV `invites` | authoritative hub JetStream replicas | direct KV | KV create/update to hub quorum |
| Deploy commits | JetStream stream `deploy_commits` | authoritative hub replicas | daemon projection from stream | append one immutable commit to hub quorum |
| Deploy status | NATS KV `deploy_status` | authoritative hub replicas | direct KV/projection | mutable KV update to hub quorum |
| Instance status | NATS KV `instances` | authoritative hub replicas | routing projection or direct KV | participant writes status to hub quorum |
| Routing snapshot | local projection | each gateway/DNS/daemon process memory | in-process memory | rebuilt from authoritative NATS state |
| Routing events | JetStream stream `routing_events` | authoritative hub replicas | durable or temporary consumer | atomic batch publish to hub quorum |
| Certificates metadata | NATS KV `certificates` | authoritative hub replicas | direct KV/subscription | KV put to hub quorum |
| Certificate PEM blobs | NATS Object Store | authoritative hub replicas | object get, often cached by consumers | object put to hub quorum |
| ACME challenges | NATS KV `acme_challenges` | authoritative hub replicas | gateway/cert reader projection | KV put/delete to hub quorum |
| Locks/leases | NATS KV `locks` | authoritative hub replicas | direct KV only for diagnostics | CAS create/update/delete to hub quorum |
| Cert jobs | JetStream work queue | authoritative hub replicas | worker pull consumer | publish to hub quorum, ack to hub quorum |
| Scheduled renewals | JetStream scheduled message | broker-owned in hub | delivered to work queue at due time | scheduled publish to hub quorum |
| Node commands | core NATS request/reply | not durable; target daemon subscription | request travels to target daemon | reply from target daemon |
| ZFS datasets/snapshots | local node disk | node that owns the volume | local zfs command or node RPC metadata | local zfs command; transfer bytes over TCP |
| Mirror read copies | mirror streams/KV in leaf domain | mirror node local disk | local mirror read | async replication from hub; not write authority |

The important split:

- Durable service projections use stable per-machine consumers.
- Short-lived watches use temporary consumers and do not leave durable cursor
  state behind.

- **authority** lives in hub JetStream/KV/Object Store,
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
| NATS request/reply to offline node | core NATS no responder or timeout | subscription absence or timeout | foreground failure; caller decides retry/abort |
| Work queue dispatch | JetStream write + consumer delivery | stream write plus consumer pull/ack | exactly-one-worker behavior, not zero-latency signaling |
| Scheduled message | broker schedule | schedule time plus dispatch latency | daemon restart does not lose timer |
| Mirror read | local mirror stream/KV | mirror lag and local read | fast but not authoritative for writes |
| ZFS transfer | direct TCP byte stream | bandwidth, RTT, disk, compression | control may be NATS; payload is streaming TCP |

### Internal Flows

#### Deploy apply

Data touched:

1. `locks.deploy.<namespace>` KV CAS acquire.
2. Current deployment/routing state read from local projection or hub.
3. Participant runtime commands over NATS request/reply.
4. Participant writes `instances` status records as candidates become ready.
5. One append to `deploy_commits`.
6. `deploy_status` KV update.
7. Gateway/DNS projections reload from the commit/status state.

Latency shape:

- lock acquire: one hub quorum write,
- each participant command: request/reply RTT to that node plus runtime work,
- readiness: dominated by container startup/probe time,
- commit: one hub quorum stream append,
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
- bootstrap and self-published membership writes are hub quorum writes,
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

- NATS coordination is a few hub writes,
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
- final metadata publish is one hub quorum write.

### Common Operator Operations

| Operator operation | Expected latency shape | Notes |
|--------------------|------------------------|-------|
| `status` local | local projection + health probes | should be fast; mark freshness explicitly if projection is stale |
| `status --live` cluster | NATS request/reply fan-out or sampled probes | bounded by slow/offline targets unless output is per-node partial |
| `deploy preview` | reads + live reachability probes | should not write; fails if required live facts cannot be checked |
| `deploy apply` | deploy lock KV write + participant commands + commit stream write | lock/commit pay quorum; runtime start/readiness dominates total time |
| `machine add` | bootstrap out-of-band + membership write | does not pay storage-promotion cost |
| `storage promote R=3 plan` | live probes + NATS metadata reads | read/probe-heavy; no mutation until apply |
| `storage promote R=3 apply` | stream/KV reconfiguration and catch-up | proportional to data size and slowest selected candidate catch-up |
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

### Leaf And Mirror Nodes

Leaf nodes do not vote in Raft. A leaf command has request/reply latency to the
leaf daemon, but authoritative writes still route to the hub and pay hub quorum
latency.

Mirror nodes can make reads local, but mirror data is not the write authority.
Any workflow that treats mirror state as a correctness boundary must explicitly
account for mirror lag.

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
- `regional_mirror_read_local_write_owner_explicit`
- `region_failover_requires_operator_promotion`

Tests should assert NATS-visible behavior where that is the product contract:
stream replica count, KV/stream write failure, request/reply no responders,
consumer catch-up, mirror lag, and operator-visible status. Direct TCP assertions
belong only to bulk transfer scenarios.
