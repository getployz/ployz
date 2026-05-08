# Authority Roadmap

Ployz authority is about ownership, not geography.

Core rules:

- An authority owns durable control-plane truth.
- A region is placement, latency, routing, and failure-domain metadata.
- One authority can span many regions.
- Many regions do not imply many authorities.
- NATS is the substrate. Authority is the product model.
- Remote mutations never queue. If the target authority is unreachable, fail now.
- No automatic failover rewrites authority ownership.
- Volume replication is out of scope here. This doc covers control-plane durability.

## Data Buckets

| Bucket | Meaning | Examples | Disposable |
| --- | --- | --- | --- |
| Stored intent | Operator-written truth and lifecycle events | machine records, deploy commits, instance records, cert metadata | Only if HA keeps quorum |
| Projection | Derived view rebuilt from stored intent | gateway table, DNS view, routing event stream | Yes, after rebuild |
| Live facts | Observed now by request/reply or local probe | node commands, capacity offers, no-responder | Yes |
| Health metrics | Status-only observations | stream lag, replica health, listener freshness | Yes; never write back as truth |

Do not promote health metrics or projections into stored truth.

## Authority Lifecycle

| Stage | Shape | What fails |
| --- | --- | --- |
| Bootstrap | One cloud node, `auth-default`, R=1 | Lose it, lose control-plane truth |
| More nodes | Candidates, compute, gateway, DNS | Lose them, control plane keeps its truth |
| HA | Explicit R=3 or R=5 storage promotion | Lose quorum, authority stops writes |
| DR | Async mirrors/read-local copies | Mirror lag defines loss window |
| Multi-authority | Explicit ownership split | Cross-authority writes fail when target is unreachable |

Storage durability does not create a new authority. Regional ownership does.

## HA and DR

HA and DR are independent switches per authority.

| Posture | Meaning | Operator promise |
| --- | --- | --- |
| pre-HA, no-DR | R=1, no mirror | Simple, not durable against authority-node loss |
| pre-HA, DR | R=1 plus async mirror | Writes still stop if authority dies; manual recovery may exist |
| HA, no-DR | R=3/R=5, no mirror | Survives storage-member loss inside the authority |
| HA, DR | R=3/R=5 plus mirror | Survives member loss and has an explicit disaster path |

HA means control-plane truth survives node loss. It does not mean user volumes
are replicated.

## Node Roles

| Role | Bucket | Control-plane loss impact |
| --- | --- | --- |
| Authority storage | Stored intent | R=1: not disposable. R=3/R=5: disposable only while quorum remains. |
| Storage candidate | Stored intent membership | Disposable for control-plane truth; may still host workloads. |
| Compute | Live facts plus stored membership | Disposable for control-plane truth; workload impact is separate. |
| Gateway | Projection | Disposable; traffic shifts after routing view rebuilds. |
| DNS | Projection | Disposable; name resolution shifts after view rebuilds. |
| Dev authority | Stored intent, later | Owns local truth; remote writes fail when remote authority is unreachable. |

`ployzd` is disposable. NATS, WireGuard, gateway, DNS, and workloads are data
plane and should outlive daemon restart.

## Current NATS Assets

| Asset | Kind | Scope | Bucket | Loss impact |
| --- | --- | --- | --- | --- |
| `machines_<installation>` | KV | installation root | Stored intent | Cluster membership and machine identity unavailable/lost. |
| `cp_deploy_commits_<authority>` | stream | authority | Stored intent | Deploy truth/history unavailable/lost. |
| `cp_deploy_status_<authority>` | KV | authority | Stored intent | Deploy lifecycle status unavailable/lost. |
| `cp_instances_<authority>` | KV | authority | Stored intent | Runtime lifecycle/routing source unavailable/lost. |
| `cp_invites_<authority>` | KV | authority | Stored intent | Invite validation unavailable/lost. |
| `cp_acme_accounts_<authority>` | KV | authority | Stored intent | ACME account material unavailable/lost. |
| `cp_certificates_<authority>` | KV | authority | Stored intent | Certificate material unavailable/lost. |
| `cp_acme_challenges_<authority>` | KV | authority | Stored intent | Active challenge state unavailable/lost. |
| `cp_acme_challenge_readiness_<authority>` | KV | authority | Stored intent | Challenge readiness unavailable/lost. |
| `routing_events_<authority>` | stream | authority | Projection | Watchers reload/rebuild routing view. |
| `cp_locks_<authority>` | KV | authority | Live facts | In-flight leases fail or expire; no business truth lost. |
| `work_cert_<authority>` | stream | authority | Projection | Certificate jobs may need regeneration from cert metadata. |

Every asset uses the authority's configured replica policy. R=1 is simple, not
disposable. R=3/R=5 is explicit operator intent.

## Regions

Regions answer "where can this run?" not "who owns truth?"

| Region role | Meaning |
| --- | --- |
| `home_data` | Default placement for authority storage. |
| `compute` | Workloads/gateway/DNS may run here; writes still go to authority. |
| `draining` | No new placement. Existing work is moved by explicit command. |
| `disabled` | Not eligible for placement. |

Promote a region to its own authority only when it needs local writes during a
partition, an ownership boundary, failure isolation, or dev/team autonomy.

## Roadmap

1. Docs: keep this page as the single story. Delete old NATS topology docs.
2. Status: show node role, failure impact, asset bucket, replica state, and lag.
3. Single authority: machine add never changes authority or replica count.
4. HA: add explicit R=3/R=5 storage promotion.
5. Compute regions: global placement, one owning authority.
6. DR: async mirrors with visible lag and manual promotion.
7. Multi-authority/dev: explicit ownership split; no queued remote intent.

Stop at any tier that gives enough value.
