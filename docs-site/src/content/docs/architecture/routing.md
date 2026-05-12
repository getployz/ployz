---
title: "Routing, gateway, and DNS: how Ployz serves traffic"
description: "How Ployz models deploy truth as durable committed facts, projects routing state to the gateway and DNS, and defines what makes an instance routable."
llms:
  summary: "How Ployz models deploy truth as durable committed facts, projects routing state to the gateway and DNS, and defines what makes an instance routable."
---
Routing and deploys in Ployz follow a single baseline rule: traffic only sees committed, routable facts. The gateway and DNS are projections rebuilt from durable state, not authoritative stores of their own. This page explains the deploy truth model, how an apply flows through to a final commit, and what "routable" means in practice.

## Deploy truth model

One owning authority accepts durable deploy writes. All deploy state belongs to that authority, even when workloads run across multiple regions or machines. Regions affect placement decisions — `draining` and `disabled` regions do not receive new placements — but they do not create additional write authorities.

The following table describes every kind of state Ployz tracks for deploys and routing, how it is stored, and what it is used for.

| Data                     | Kind              | Notes                                                                                                                                                                                                                                     |
| ------------------------ | ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Deploy commits           | Durable intent    | Immutable events appended to `cp_deploy_commits_<authority>`. Each commit is a point of no return for the facts it contains.                                                                                                              |
| Deploy status            | Durable status    | Mutable lifecycle record in `cp_deploy_status_<authority>`. Transitions through `applying`, `committed`, `failed`, and `FailedAfterCheckpoint`.                                                                                           |
| Deploy phase records     | Durable status    | Per-phase execution state, work, policies, and commit linkage in `cp_deploy_phases_<authority>`.                                                                                                                                          |
| Branch lineage           | Durable intent    | Committed service source lineage folded from deploy commits. Explains which source revision a target service came from.                                                                                                                   |
| Volume movement evidence | Durable intent    | Committed volume source/target and verified transfer proof folded from deploy commits. Explains which deploy and phase moved a volume, which machines were involved, and which verified transfer snapshot made the ownership change safe. |
| Instance records         | Durable status    | Runtime lifecycle in `cp_instances_<authority>`.                                                                                                                                                                                          |
| Routing events           | Projection        | Ordered facts in `routing_events_<authority>`. Rebuildable from stored intent. The gateway and DNS consume these to build their projections.                                                                                              |
| Placement probes         | Live facts        | NATS request/reply. No responder means the machine is unavailable right now. Not stored.                                                                                                                                                  |
| ZFS transfer progress    | Live facts        | Foreground operation evidence while a transfer is running. Only verified success folded into a deploy commit becomes durable movement evidence.                                                                                           |
| Deploy lock              | Live coordination | Lease in `cp_locks_<authority>`. Prevents concurrent deploys to the same namespace.                                                                                                                                                       |

:::note
Raw manifests are not stored as deploy evidence because service specs may contain sensitive values. Branch lineage and volume movement evidence are committed facts, not routing inputs.
:::

## The apply flow

A deploy apply is foreground work with nine explicit steps. Each step has a defined scope and a defined failure behavior.

### Preview manifest against current stored intent

  The orchestrator reads current deploy commits, status, and instance records to understand what is already running. This produces a plan: which instances need to start, which need to stop, which volumes need to move.

### Acquire the namespace deploy lease

  The orchestrator acquires a deploy lock in `cp_locks_<authority>` for the target namespace. This prevents a second concurrent apply from starting. If the lock is already held, the apply fails immediately with a structured error.

### Probe eligible machines for live capacity

  For each candidate machine, the orchestrator sends a NATS request/reply to the machine's command subject. The response includes current capacity. No responder or timeout marks the machine as unavailable for this deploy.

### Write applying deploy status and pending phase records

  The deploy status is written as `applying`. Per-phase records are written with `pending` state. These are the first durable writes of the apply.

### Execute phase-owned work

  For each phase: stop moved-volume writers on the source machine, perform any blocking ZFS moves, then start candidate containers on target machines and wait for readiness.

### Append checkpoint commits for intermediate phases

  For checkpoint phases, append an immutable deploy commit containing the facts owned by that phase. Link the phase record to the commit ID. After a checkpoint commit, later failure is reported as `FailedAfterCheckpoint` — the checkpointed facts remain durable.

### Append the final deploy commit

  Append the final immutable deploy commit for all remaining facts. Link end-of-deploy phase records to the final deploy ID. This is the point of no return. Before this commit, failure aborts with no lasting state change. After it, the new version is live.

### Publish derived routing events

  Publish ordered routing events to `routing_events_<authority>`. The gateway and DNS consume these events to update their projections.

### Drain and remove old instances

  Old instances are drained and stopped. Cleanup failures are recorded as explicit recoverable status — they do not erase the fact that the new version is live and do not revert the committed deploy.

:::caution
Before the first commit, a failure aborts the deploy and leaves no lasting state change. After the final commit, cleanup failures are visible status — not deploy failure. An operator can inspect and repair cleanup without rolling back the deploy.
:::

## Routing projection

The gateway and DNS are stateless projections. They do not hold authoritative routing state; they rebuild it from durable records.

On startup:

1. Load stored routing state from the store (machines, revisions, releases, instances).
2. Then consume ordered routing events from the stream, applying each event to update the in-memory projection.
3. Begin serving.

While running, the gateway and DNS continue consuming routing events as they arrive. Each event is applied in order: upserts replace the existing record for a contract identity, removals drop the matching record.

**If freshness becomes uncertain** — for example, if the consumer falls behind or loses its position in the stream — the projection discards its local view entirely and rebuilds from stored state, then resumes consuming events. This is a deliberate safety property: serving a stale projection silently is worse than the brief interruption of a rebuild.

:::note
Routing events are rebuildable projections. They are published as a convenience for consumers that want an ordered stream of changes. The source of truth is always the durable intent stored in deploy commits and instance records. If the event stream is lost, it can be reconstructed.
:::

## What "routable" means

An instance is routable when all of the following conditions hold:

* **Ready** — the instance has completed startup and passed its readiness check
* **Not draining** — the instance is not in the process of being stopped for a migration or deploy
* **No errors** — no unrecovered error state recorded for the instance
* **Has overlay IP** — the instance has a reachable overlay network address
* **Matches current slot, machine, and revision** — the instance belongs to the current committed release, not a superseded one

An instance that fails any condition is excluded from the gateway's upstream pool and from DNS records. There is no grace period or optimistic inclusion. The commit that made a new revision live also makes old instances eligible for removal, but those old instances remain in the projection until they are explicitly drained and their routing events are published.

## Remote commands via NATS request/reply

Participant actions during an apply — starting a candidate container, stopping a writer before a volume move, checking capacity — use NATS request/reply on per-machine subjects. Each command targets an explicit machine. There are no broadcast commands and no session state.

```text
ployz.v1.<installation>.<authority>.rpc.node.<machine_id>.<command>
```

No responder and timeout fail the foreground operation. The orchestrator does not retry silently — it returns a structured `RpcFailure` to the caller. The caller or operator decides whether to retry the whole operation.

:::tip
Because commands target explicit machines rather than creating sessions, the orchestrator can restart between steps of a multi-machine operation without losing the ability to issue the next command. Each command is independently targeted.
:::
