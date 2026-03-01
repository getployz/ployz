# Corrosion Health and Prometheus Readiness Reference

This document captures the Corrosion health signals we use for integration and gating decisions.

## HTTP health endpoint

Use the Corrosion API health endpoint with explicit thresholds:

```text
GET /v1/health?gaps=0&p99_lag=5.0&queue_size=100
```

Behavior:

- `200` when current metrics satisfy the requested thresholds.
- `503` when thresholds are not met.

Example response body:

```json
{
  "response": {
    "gaps": 0,
    "members": 3,
    "p99_lag": 0.12,
    "queue_size": 0
  }
}
```

### Response fields

| Field | Meaning | Ready target |
| --- | --- | --- |
| `gaps` | Sum of missing version ranges across actors (from Corrosion bookkeeping gaps) | `0` |
| `members` | Alive peers discovered by gossip | `>= expected cluster size` |
| `p99_lag` | Rolling 99th percentile commit lag | low (example `< 5s`) |
| `queue_size` | Pending changes not yet applied locally | `0` or near-zero |

## Cold-start caveat

A fresh node can report `gaps=0` before gossip converges.

Typical sequence:

1. `members=1`, `gaps=0` -> node has not discovered peers yet (not ready).
2. `members=N`, `gaps>0` -> node discovered peers and is learning missing ranges (not ready).
3. `members=N`, `gaps` decreasing -> active catch-up (not ready).
4. `members=N`, `gaps=0`, `queue_size=0` -> caught up (ready).

Always combine health checks with `members >= expectedMembers`.

## Prometheus metrics

Metrics loop cadence is approximately every 10 seconds.

### Replication progress

| Metric | Type | Labels | Meaning |
| --- | --- | --- | --- |
| `corro_db_gaps_sum` | gauge | `actor_id` | Missing version count per peer; all zero means caught up |
| `corro_db_buffered_changes_rows_total` | gauge | `actor_id` | Changes received but not yet applied, per peer |
| `corro_sync_client_head` | gauge | `actor_id` | Highest version we have from each peer |
| `corro_sync_client_needed` | gauge | `actor_id` | Missing version ranges needed from each peer; zero means caught up with that peer |
| `corro_agent_changes_commit_lag_seconds` | histogram | none | End-to-end lag from change creation to local commit |
| `corro_agent_changes_recv_lag_seconds` | histogram | `source` | Lag measured at receive time (`local`/`remote`) |

### Sync activity

| Metric | Type | Labels | Meaning |
| --- | --- | --- | --- |
| `corro_sync_changes_recv` | counter | `actor_id` | Changes pulled via sync protocol |
| `corro_sync_changes_sent` | counter | `actor_id` | Changes pushed to peers |
| `corro_sync_client_req_sent` | counter | `actor_id` | Sync requests sent |

### Cluster health

| Metric | Type | Labels | Meaning |
| --- | --- | --- | --- |
| `corro_gossip_members` | gauge | none | Total known members |
| `corro_gossip_cluster_size` | gauge | none | Reported cluster size |
| `corro_agent_changes_in_queue` | gauge | none | Pending changes in memory queue |

## Readiness policy (recommended)

Use aggregate PromQL checks for gates:

```promql
sum(corro_db_gaps_sum) == 0
and sum(corro_sync_client_needed) == 0
and corro_gossip_members >= <expected_members>
and corro_agent_changes_in_queue == 0
```

Operational guidance:

- Require the readiness condition to hold for multiple consecutive polls to avoid flapping.
- For quiesced writes, the condition above is a hard catch-up signal.
- For active writes, this is a moving target; pair with lag thresholds for service SLOs.

## Suggested state mapping for gating

When integrating into runtime state machines (for example, Ployz), keep the state model minimal and source it directly from Corrosion:

- `unreachable`: health/metrics endpoint cannot be read.
- `forming`: endpoint reachable, but `members < expected`.
- `syncing`: members ready, but `gaps`, `needed`, or queue still non-zero.
- `ready`: members ready and replication checks hold for consecutive polls.

Use this state for workload/control-plane gating; treat ad-hoc lag views as diagnostics only.

## Ployz Bootstrap Policy

Ployz uses a binary startup gate based on WireGuard peer reachability:

**Has reachable peers (Alive or New)?** Wait for Corrosion `gaps=0` (2 consecutive passes at 2s intervals). Reachable peers can serve all missing data, so gaps will resolve.

**No reachable peers?** Ready immediately (2 consecutive passes). There is no one to sync from — gaps from dead/removed actors are unresolvable.

### Why gaps (not queue_size)

`gaps=0` is the true sync-completion signal. `queue_size=0` can be transiently true before all ranges have been requested. Gaps tracks what the node knows it is missing.

### Why not members >= N

A strict member count threshold fails when a node restarts after a peer has been removed. The dead peer never responds to gossip, so `members >= expected` is never satisfied. Instead, Ployz probes WireGuard handshakes to determine which peers are actually reachable and adjusts expectations accordingly.

### Why skip the gate when alone

When no peers are reachable (all Suspect), any gaps are from actors whose data no alive peer can provide. Waiting would block forever. The single-node and "peer removed while down" cases both hit this path.
