# Routing and Deploys

Routing and deploys use the authority model in
[`authority-roadmap.md`](authority-roadmap.md).

## Deploy Truth

One owning authority accepts durable deploy writes.

| Data | Bucket | Notes |
| --- | --- | --- |
| Deploy commits | Stored intent | Immutable event appended to `cp_deploy_commits_<authority>`. |
| Deploy status | Stored intent | Mutable lifecycle in `cp_deploy_status_<authority>`. |
| Instance records | Stored intent | Runtime lifecycle in `cp_instances_<authority>`. |
| Routing events | Projection | Ordered facts in `routing_events_<authority>`. Rebuildable from stored intent. |
| Placement probes | Live facts | NATS request/reply. No responder means unavailable now. |
| Deploy lock | Live facts | Lease in `cp_locks_<authority>`. Coordination only. |

Regions affect placement. They do not create write authority. Deploy planning
may place workloads in `home_data` and `compute` regions; `draining` and
`disabled` regions do not receive new placements. Deploy commits, status,
instance records, and routing events still belong to the owning authority.

## Apply Flow

1. Preview manifest against current stored intent.
2. Acquire one namespace deploy lease in the owning authority.
3. Probe eligible machines for live capacity.
4. Start candidate containers and wait for readiness.
5. Append one immutable deploy commit.
6. Publish derived routing events.
7. Drain and remove old instances.

The commit is the point of no return. Before commit, failure aborts. After
commit, cleanup failure is visible state, not deploy failure.

## Routing

Gateway and DNS are projections.

- On startup, load stored intent.
- Then consume ordered routing events.
- If freshness is uncertain, discard local view and rebuild.
- Do not store health back as truth.

Routable means ready, not draining, no errors, has overlay IP, and matches
current slot/machine/revision records.

## Remote Commands

Small participant actions use NATS request/reply on per-machine subjects.
Commands target explicit machines and do not create sessions.

No responder and timeout fail the foreground operation. The caller or operator
decides whether to retry.
