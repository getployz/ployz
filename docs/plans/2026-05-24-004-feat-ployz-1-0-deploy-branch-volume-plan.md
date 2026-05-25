---
title: "feat: Build Ployz 1.0 Deploy, Branch, Rolling, And Volume Primitives"
type: feat
status: draft
date: 2026-05-24
origin:
  - VISION.md
  - docs/architecture/deploy-primitives-roadmap.md
  - docs/routing-and-deploys.md
  - docs/plans/2026-05-10-003-feat-deploy-volume-snapshot-clone-branching.md
  - docs/plans/2026-05-24-002-feat-ployz-1-0-cli-shape-and-workflows-plan.md
  - /Users/nick/dev/uncloud/pkg/client/deploy
  - /Users/nick/dev/uncloud/pkg/client/compose
---

# feat: Build Ployz 1.0 Deploy, Branch, Rolling, And Volume Primitives

## Summary

Build the product engine behind these 1.0 workflows:

- `ployz deploy preview/apply`
- `ployz branch preview/create/update/delete`
- `ployz promote preview/apply`
- `ployz rollback preview/apply`
- `ployz volume fork/move/snapshot`
- `ployz machine drain/remove`

Deploy is the compiler. Branch, promote, rollback, migrate, and drain are
front-ends that resolve intent into deploy phases plus volume/routing work.

Copy the simple shape from `~/dev/uncloud`:

- product spec validates itself;
- current cluster state is inspected once for planning;
- planner returns typed operations;
- human output renders the operations;
- apply executes operations in sequence or bounded batches;
- health/rollback behavior lives beside the operation that needs it.

Do not copy generic workflow engines, hidden controllers, or private cloud-only
semantics.

## Product Model

### Manifest

The 1.0 manifest should describe product intent, not backend commands:

```yaml
namespace: prod
services:
  api:
    image: registry/app/api:sha256-...
    ports:
      - name: http
        target: 8080
    routes:
      - host: api.example.com
    replicas: 2
    readiness:
      http: /health
    volumes:
      - data:/var/lib/api
    rollout:
      strategy: rolling
      max_unavailable: 1
volumes:
  data:
    backend: zfs
```

Branch source policy is an overlay, not a separate manifest type:

```yaml
branch:
  from: prod
  resources:
    services:
      api:
        source: fresh
      web:
        source: branch:pr-221/web
    volumes:
      data:
        source: clone:prod/data
        data_policy: raw
        consistency: crash_consistent
```

### Internal Intent Types

```text
DeployIntent
  namespace
  service_changes
  volume_changes
  route_changes
  rollout_policy
  source_policy
  deadline

ServiceChange
  create_revision
  keep_revision
  remove_service
  scale

VolumeChange
  create_empty
  fork_from_snapshot
  move_owner
  attach
  detach
  delete_after_cleanup

RouteChange
  create
  switch_target
  remove
```

Keep these as Ployz product types. They should not expose Corrosion, iroh,
WireGuard, ZFS command details, or SQL rows.

## Planner Model

Do not start with a generic workflow engine. The first implemented plan is a
single-service deploy plan:

```text
SingleServiceDeployPlan
  plan_id
  namespace
  baseline
  warnings
  preflights
  phases
  commit_points
  rollback_point
```

Initial phase kinds:

```text
ProbePeers
EnsureImage
StartInstance
WaitReadiness
PromoteRoute
DrainInstance
CleanupInstance
CommitRows
Verify
```

Add `CreateVolume`, `SnapshotVolume`, `CloneVolume`, `MoveVolume`,
`StopInstance`, and other phase kinds only when the next slice consumes them.
Branch, promote, rollback, migrate, and drain can reuse the same phase structs
after they exist; they should not force a general DAG abstraction up front.

### Plan Baseline

The plan baseline records exactly what apply must revalidate:

- Corrosion EOQ/change point or equivalent query snapshot marker;
- primary keys and row digests for rows used by the plan;
- live probe receipts with endpoint ID, peer machine ID, observed capability or
  readiness, deadline, and probe time;
- operation-specific revalidation rules.

Apply rejects before mutation when any baseline rule fails. Examples:

- route promotion revalidates route rows, candidate placement, and fresh
  readiness;
- volume fork revalidates source owner, snapshot/watermark, and target
  non-existence;
- machine drain revalidates affected placement and volume owner sets.

Planning algorithm:

1. Ask the reached coordinator for its current observed state of the target
   namespace: running instances, volumes, routes, and durable evidence rows it
   can see.
2. Resolve authority island candidate machines.
3. Probe only peers needed by the operation.
4. Resolve images, service revisions, current placements/observations, volumes,
   and routes.
5. Build candidate operations.
6. Attach preconditions and warnings.
7. Build the plan baseline for rows and live facts that must stay stable until
   apply.

Apply algorithm:

1. Re-read and re-probe preconditions.
2. Reject drift that invalidates the preview.
3. Execute phases.
4. Commit rows only at defined checkpoints.
5. Preserve side-effect evidence.
6. Report exact state on failure.

## Single-Service Deploy MVP

Command:

```text
ployz deploy apply -f ployz.yaml --namespace prod
```

Scope:

- one namespace;
- one service;
- one or more replicas;
- image already available or explicitly distributed;
- optional fresh ZFS volume;
- one HTTP route.

Implementation units:

### U1. Product Spec And Validation

Files:

- `crates/ployz/src/deploy/mod.rs`
- new `crates/ployz/src/deploy/spec.rs`
- new CLI crate when introduced

Work:

- Define manifest/service/volume/route/rollout types.
- Add defaults and validation on the product types.
- Add `preview` rendering model separate from terminal rendering.

Acceptance:

- Invalid manifests fail before touching peers.
- JSON output contains the same plan data human output renders.

### U2. Cluster Planning State

Files:

- `crates/ployz/src/deploy/planning.rs`
- `crates/ployz/src/adapters/polis/deployment_store.rs`

Work:

- Query machines, namespaces, service revisions, service instance placements,
  service instance observations, volumes, and routes.
- Probe candidate machines through runtime RPC.
- Build an immutable `ClusterPlanningState`.

Acceptance:

- Unit tests can plan from an in-memory planning state without Corrosion.

### U3. Typed Operations

Files:

- `crates/ployz/src/deploy/operation.rs`
- runtime RPC adapter

Work:

- Add small operations with `preview`, `execute`, `baseline`, and structured
  failure.
- Start with `EnsureImage`, `StartInstance`, `WaitReadiness`, `PromoteRoute`,
  `DrainInstance`, `CleanupInstance`, `CommitRows`.

Acceptance:

- A failed readiness phase stops before route promotion and returns retryable
  evidence.

## Rolling Deploy

Command:

```text
ployz deploy apply -f ployz.yaml --strategy rolling
```

Rules:

- Start new candidate before stopping old when ports/volumes allow it.
- Use stop-first for single-writer volume cases unless explicit policy says
  otherwise.
- Route inclusion happens only after candidate readiness.
- Old instance drain happens after route inclusion.
- Cleanup failure after route switch is a follow-up, not a full rollback.

Implementation units:

### U4. Rolling Strategy Planner

Work:

- Port uncloud's useful strategy shape:
  - inspect current instances;
  - sort up-to-date instances first;
  - choose eligible machines;
  - plan create/replace/remove operations;
  - choose start-first or stop-first from ports and volume writer semantics.
- Replace Docker container details with Ployz runtime instance details.

Acceptance:

- Tests cover no-op, scale up, scale down, image change, port conflict,
  single-writer volume, and partially failed previous deploy cleanup.

### U5. Route Step Checkpoints

Work:

- Add checkpoint and baseline revalidation before traffic-affecting route
  switch.
- Add checkpoint after route switch with current active instance set.
- Add verify path that can inspect route rows and live gateway projection.

Acceptance:

- Failed after route switch prints exact live route and cleanup next action.

## Volumes

Commands:

```text
ployz volume create prod/data
ployz volume snapshot prod/data
ployz volume fork preview prod/data pr-219/data
ployz volume fork apply prod/data pr-219/data
ployz volume move preview prod/data --to node-c
ployz volume move apply prod/data --to node-c
```

Volume rules:

- ZFS is the first-class 1.0 backend.
- Empty/fresh volume creation is safe and simple.
- Fork creates a new identity from a source snapshot.
- Move preserves identity and changes owner after verified transfer.
- Portal/live attach is denied until explicit safety policy exists.

Implementation units:

### U6. ZFS Local Clone Backend

Files:

- runtime backend crate/module
- `crates/ployz/src/volume/mod.rs`

Work:

- Define first backend operations:
  - ensure dataset;
  - snapshot;
  - clone local snapshot;
  - verify dataset;
  - destroy provisional local clone.

Acceptance:

- Local ZFS test proves snapshot, clone, divergent writes, and cleanup.

### U7. Volume Fork

Work:

- Resolve source volume and source owner.
- Probe source machine.
- Snapshot source.
- Clone target on same machine for v1.
- Commit target volume and snapshot lineage.

Acceptance:

- `ployz volume fork apply prod/data pr-1/data` creates independent target
  data and records lineage without changing source ownership.

### U8. Volume Move

Work:

- Use the current owner machine as the transfer fence for the source volume.
- RPC to the current owner machine for all source-side writes; the owner
  serializes stop-writes, snapshot, and source watermark updates locally.
- Stop or drain writers and record a stop-writes receipt written by the owner.
- Snapshot the named base on the owner.
- Send to a provisional target dataset with a deterministic artifact id.
- Resume or restart receive by artifact id on the target.
- Apply final delta from the named base.
- Verify target dataset identity, source watermark, and backend receipt.
- Activate target and have the target write the final volume owner row.
- Cleanup source/provisional artifacts idempotently, or report follow-up with
  artifact ids.

Acceptance:

- A move that fails before commit leaves source owner unchanged.
- A move that fails after commit reports target owner plus cleanup work.

## Branch

Commands:

```text
ployz branch preview pr-219 --from prod -f ployz.yaml
ployz branch create pr-219 --from prod -f ployz.yaml
ployz branch update pr-219 -f ployz.yaml
ployz branch delete preview pr-219
ployz branch delete apply pr-219
```

Branch rules:

- A branch is a namespace plus source lineage plus deploy commits.
- Source policy is per resource.
- Supported source modes for 1.0:
  - `fresh`;
  - `clone:<namespace>/<volume>`;
  - `omit`.
- Reserved but rejected:
  - `branch:<namespace>/<resource>` for mixed-source composition;
  - `portal`;
  - `shared_read_only`;
  - provider-native branches.

Implementation units:

### U9. Branch Compiler

Work:

- Resolve source namespace and target namespace.
- Resolve per-resource source policy.
- Compile service revisions, volume fork work, route allocation, and deploy
  phases.
- Write namespace rows and branch lineage at commit.

Acceptance:

- Branch preview shows services, volumes, route hostnames, clone sources,
  omitted resources, and unsupported source modes.

### U10. Branch Delete

Work:

- Compile route removal, instance stop, volume cleanup, and namespace
  tombstone.
- Do not run hidden garbage collection.

Acceptance:

- Delete reports any cleanup failures as follow-up work and leaves enough rows
  for operator recovery.

### Deferred: Multi-Source Branch

Work:

- Allow each service and volume to resolve from a different source namespace.
- Preserve source metadata in deploy commit evidence.

Acceptance:

- This is not required for 1.0. It becomes viable after single-source branch,
  promotion, rollback, and lineage evidence are boring.

## Promote And Rollback

Commands:

```text
ployz promote preview pr-219 --to prod
ployz promote apply pr-219 --to prod
ployz rollback preview prod --to dep_123
ployz rollback apply prod --to dep_123
```

Rules:

- Promote compiles a production deploy from branch lineage and current
  production truth.
- Promote does not copy branch rows over production rows.
- Rollback compiles a deploy from an immutable previous deploy commit.
- Volume rollback must state data-loss and lineage caveats before apply.

Implementation units:

### U11. Promotion Compiler

Work:

- Compare branch deploy commit to production.
- Resolve route switch, service revision changes, volume source policy, and
  rollback point.
- Apply with normal deploy phase discipline.

Acceptance:

- Promotion preview clearly shows what production will run and what rollback
  point will remain.

### U12. Rollback Compiler

Work:

- Reconstruct target service/route/volume state from deploy commit evidence.
- Reject missing irreversible volume evidence.
- Compile normal deploy phases.

Acceptance:

- Rollback of stateless services works first.
- Stateful rollback is rejected until volume snapshot evidence is sufficient.

## Machine Drain And Remove

Commands:

```text
ployz machine drain preview node-a --all
ployz machine drain apply node-a --all
ployz machine remove preview node-a
ployz machine remove apply node-a
```

Rules:

- Drain compiles service moves and volume moves.
- Remove tombstones only after drain verification.
- Drain is foreground and visible.

Implementation units:

### U13. Drain Compiler

Work:

- List service instance placements and volume ownership on machine.
- Compile replacement placement and volume movement.

Acceptance:

- Drain preview shows every affected service, volume, route, and machine.

### U14. Remove

Work:

- Verify machine has no active placements.
- Tombstone machine row.
- Rebuild peer derivation.

Acceptance:

- Remove refuses active workloads and gives the exact drain command.

## Roadmap Dependencies

Required before this plan:

- Corrosion store primitive.
- Iroh peer RPC.
- Machine membership and namespace rows.
- Runtime backend start/stop/verify.

Dependency order:

1. Single-service deploy MVP.
2. Deploy evidence rows and plan baselines.
3. Route checkpoints.
4. Fresh volume create.
5. ZFS local snapshot/clone.
6. Branch fresh mode.
7. Branch with cloned volume.
8. Rolling deploy.
9. Promote stateless branch.
10. Rollback stateless deploy.
11. Volume move with owner-machine serialization.
12. Machine drain/remove.
13. Stateful promote/rollback once volume evidence is sufficient.

## Test Matrix

- Pure planning tests for every workflow.
- Snapshot tests for human plan rendering.
- JSON schema/output tests for agent/cloud consumers.
- Local two-node runtime deploy test.
- Local ZFS clone/fork/move tests.
- Route promotion failure tests.
- Daemon restart during deploy phase tests.
- Failed-before-commit and failed-after-checkpoint tests.

## Non-Goals

- No generic workflow DAG engine.
- No hidden PR environment reconciler.
- No portal/live production attachment in 1.0.
- No provider-native database branch backend in open-core 1.0.
- No global locks by default.
- No compatibility shims for old legacy command shapes unless a concrete
  rollout requires them.
