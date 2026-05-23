# Routing and Deploys

Routing and deploys use the authority model in
[`authority-roadmap.md`](authority-roadmap.md).

## Deploy Truth

One owning authority accepts durable deploy writes.

| Data | Bucket | Notes |
| --- | --- | --- |
| Deploy commits | Stored intent | Immutable event appended to `cp_deploy_commits_<authority>`. |
| Deploy status | Stored intent | Mutable lifecycle in `cp_deploy_status_<authority>`. |
| Deploy phase records | Stored intent | Per-phase execution state, work, policies, and commit linkage in `cp_deploy_phases_<authority>`. |
| Branch lineage | Stored intent | Committed service source lineage folded from deploy commits. |
| Volume movement evidence | Stored intent | Committed volume source/target and verified transfer proof folded from deploy commits. |
| Instance records | Stored intent | Runtime lifecycle in `cp_instances_<authority>`. |
| Routing events | Projection | Ordered facts in `routing_events_<authority>`. Rebuildable from stored intent. |
| Placement probes | Live facts | NATS request/reply. No responder means unavailable now. |
| ZFS transfer progress | Live facts | Foreground operation evidence while a transfer is running; only verified success folded into a deploy commit becomes durable movement evidence. |
| Deploy lock | Live facts | Lease in `cp_locks_<authority>`. Coordination only. |

Regions affect placement. They do not create write authority. Deploy planning
may place workloads in `home_data` and `compute` regions; `draining` and
`disabled` regions do not receive new placements. Deploy commits, status,
instance records, and routing events still belong to the owning authority.

## Apply Flow

1. Preview manifest against current stored intent.
2. Acquire one namespace deploy lease in the owning authority.
3. Probe eligible machines for live capacity.
4. Write the applying deploy status and pending phase records.
5. For each phase, execute phase-owned work: stop moved-volume writers, perform
   blocking ZFS moves, then start candidate containers and wait for readiness.
6. For checkpoint phases, append an immutable deploy commit for phase-owned
   facts and link the phase record to that commit id.
7. Append the final immutable deploy commit for remaining facts and link
   end-of-deploy phase records to the final deploy id.
8. Publish derived routing events.
9. Drain and remove old instances.

Each commit is a point of no return for the facts it contains. Before the first
commit, failure aborts. After a checkpoint commit, later failure is reported as
`FailedAfterCheckpoint` and the checkpointed facts remain durable. After the
final commit, cleanup failure is visible state, not deploy failure.

Branch lineage and volume movement evidence are committed facts, not routing
inputs. Branch lineage explains which committed source revision a target service
came from. Volume movement evidence explains which deploy and phase moved a
volume, which machines were involved, and which verified transfer snapshot made
the ownership change safe. Raw manifests are not stored as deploy evidence
because service specs may contain sensitive values.

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
