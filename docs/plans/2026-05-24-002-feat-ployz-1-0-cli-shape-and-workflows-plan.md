---
title: "feat: Define Ployz 1.0 CLI Shape And Workflows"
type: feat
status: draft
date: 2026-05-24
origin:
  - VISION.md
  - docs/architecture/deploy-primitives-roadmap.md
  - docs/architecture/functional-system-roadmap.md
  - /Users/nick/dev/uncloud
---

# feat: Define Ployz 1.0 CLI Shape And Workflows

## Summary

Design the Ployz 1.0 CLI from the operator's desired experience, then work
backward into product primitives and implementation slices.

The canonical command in this plan is `ployz`. If the shipped binary remains
`ployzctl` for a while, it should be an alias with identical behavior. The
shape below is the target product surface, not a compatibility promise for old
commands.

Ployz should feel like a direct operational tool:

1. Read the current cluster view from the reached node.
2. Probe the live peers and resources required by the command.
3. Compile a concrete plan.
4. Print the plan.
5. Apply only after approval, unless `--yes` is supplied.
6. Report phases, checkpoints, failures, and retry guidance.

This copies the useful simplicity from `~/dev/uncloud`: direct commands,
typed product specs, plan/confirm/execute flows, and small client interfaces.
It deliberately does not copy hidden background policy loops or a generic store
framework.

## CLI Principles

- One command is one foreground operation with bounded effects.
- Every mutating command has a preview path.
- `apply` is explicit for high-risk commands; convenience commands may preview
  and prompt by default.
- JSON output is first-class and stable enough for agents and cloud workers.
- Human output favors concrete phase lines over prose.
- The reached node is the coordinator for that command. It inspects its current
  view of the namespace, uses durable rows as evidence where useful, then
  probes live participants before mutation.
- Background tasks may publish observations; they do not silently rewrite
  durable cluster truth.
- Corrosion, iroh, tickets, and RPC details do not appear in normal product
  commands except under `doctor`, `debug`, or `machine ticket`.

## Global Shape

```text
ployz [GLOBAL FLAGS] <command> [COMMAND FLAGS]

Global flags:
  --cluster <name|path>          Select configured cluster context.
  --connect <target>             Connect to a node by local socket, SSH, URL, or iroh endpoint.
  --namespace <name>             Default namespace for service and volume commands.
  --format human|json|jsonl      Output format. Default: human.
  --yes                          Apply without interactive confirmation.
  --deadline <duration>          Command deadline. Default is command-specific.
  --dry-run                      Compile and print without mutation where supported.
  --verbose                      Include probes, chosen peers, and substrate timings.
```

Connection precedence:

1. Explicit `--connect`.
2. `PLOYZ_CONNECT`.
3. Current cluster context.
4. Local daemon socket.

Exit codes:

- `0`: command completed.
- `1`: invalid request or unsafe precondition.
- `2`: live peer/resource unavailable.
- `3`: command failed before durable commit.
- `4`: command failed after a durable checkpoint and needs follow-up.
- `5`: substrate unavailable or ambiguous local state.

JSON output envelope:

```json
{
  "command_id": "cmd_...",
  "kind": "deploy.apply",
  "state": "completed",
  "cluster": "prod",
  "namespace": "prod",
  "plan_id": "plan_...",
  "warnings": [],
  "phases": [],
  "commits": [],
  "next_actions": []
}
```

## Command Tree

The 1.0 primitive surface is intentionally narrow. Commands in this section
are the workflows the core must make real. Read-only inspection can be added
freely when backed by existing rows; mutating conveniences such as service
restart, service exec, service scale, generic namespace delete, and broad
volume delete stay deferred until the primitive they compile to already exists.

`machine add` is the public operator workflow. "Join" is the internal
bootstrap step performed by a daemon or by `machine ticket join` when a machine
is using an invite envelope.

Resource references accept `namespace/name`. An unqualified name resolves
against `--namespace` or the current context. Examples use qualified volume
references when crossing namespace boundaries.

High-risk mutators use explicit `preview` and `apply` subcommands. Simpler
mutators such as `machine add`, `volume create`, and `volume snapshot` still
render a preview and prompt before mutation unless `--yes` is supplied.

### 1.0 Primitive Surface

```text
ployz init
ployz status
ployz doctor

ployz machine list
ployz machine inspect <machine>
ployz machine add <target> [--name <name>] [--namespace <name>] [--ticket <ticket>]
ployz machine drain preview <machine> [--namespace <name>|--all]
ployz machine drain apply <machine> [--namespace <name>|--all]
ployz machine remove preview <machine>
ployz machine remove apply <machine>
ployz machine ticket create [--expires <duration>] [--namespace <name>]
ployz machine ticket join <ticket>

ployz namespace list
ployz namespace inspect <namespace>
ployz namespace create <namespace>
ployz namespace members <namespace>

ployz deploy preview -f <file> [--namespace <name>]
ployz deploy apply -f <file> [--namespace <name>] [--strategy rolling|all-at-once]
ployz deploy verify <deploy>
ployz deploy history [--namespace <name>] [--service <name>]
ployz deploy logs <deploy|service> [--phase <id>]

ployz service list
ployz service inspect <service>
ployz service logs <service> [--instance <id>] [--since <duration>]

ployz volume list
ployz volume inspect <volume-ref>
ployz volume create <volume-ref> [--size <size>] [--machine <machine>]
ployz volume snapshot <volume-ref> [--name <name>]
ployz volume fork preview <source-volume-ref> <target-volume-ref>
ployz volume fork apply <source-volume-ref> <target-volume-ref>
ployz volume move preview <volume-ref> --to <machine>
ployz volume move apply <volume-ref> --to <machine>

ployz branch preview <branch> --from <namespace> [-f <file>]
ployz branch create <branch> --from <namespace> [-f <file>]
ployz branch update <branch> [-f <file>]
ployz branch inspect <branch>
ployz branch delete preview <branch>
ployz branch delete apply <branch>

ployz promote preview <branch> --to <namespace>
ployz promote apply <branch> --to <namespace>
ployz rollback preview <namespace> [--to <deploy|checkpoint>]
ployz rollback apply <namespace> [--to <deploy|checkpoint>]

ployz migrate preview <service|volume|machine> --to <machine>
ployz migrate apply <service|volume|machine> --to <machine>

ployz dev up [-f <file>] [--from <namespace>] [--portal <resource>...]
ployz dev down
```

### Deferred Conveniences

```text
ployz service exec <service> -- <command...>
ployz service restart <service>
ployz service scale <service> --replicas <n>
ployz namespace delete <namespace>
ployz volume delete <volume-ref>
ployz dev sync
```

## Human Output Contract

Mutating commands render three blocks:

```text
Plan deploy api to prod

Preflights
  ok   coordinator node-a reached 3 required peers
  ok   image api:sha256:... present on node-b
  warn node-c skipped: failed live capacity probe

Phases
  1. start api@rev43 on node-b
  2. wait readiness for api@rev43
  3. add route api.example.com -> api@rev43
  4. drain api@rev42
  5. cleanup api@rev42

Result
  no changes applied yet
```

After apply:

```text
Deploy completed
  committed deploy dep_...
  active route api.example.com -> api@rev43
  old instance api@rev42 cleaned up
```

After failed-after-checkpoint:

```text
Deploy needs follow-up
  traffic is live on api@rev43
  cleanup of api@rev42 failed on node-a: peer unavailable
  retry: ployz deploy verify dep_...
```

## Typical Workflows

### 1. First Cluster

```text
ployz init --cluster prod
ployz machine add ssh://root@node-a --name node-a
ployz machine add ssh://root@node-b --name node-b
ployz status
```

Implementation implications:

- `init` creates local identity, local config, iroh key, Corrosion schema, and
  the first machine row.
- `machine add` is not a store write only. It must install/start the daemon,
  exchange iroh identity, probe RPC, write or update the machine row, and
  verify row visibility.
- The result must distinguish durable membership from live reachability.

### 2. Simple Production Deploy

```text
ployz deploy preview -f ployz.yaml --namespace prod
ployz deploy apply -f ployz.yaml --namespace prod
ployz deploy verify dep_123
```

Implementation implications:

- The deploy compiler resolves service revisions, image availability, volume
  pins, authority island candidates, route intent, and rollout strategy.
- Apply executes typed participant RPC commands; Corrosion is the row store,
  not the command bus.
- Commit rows are written only at checkpoints with enough evidence for verify
  and rollback.

### 3. Rolling Deploy

```text
ployz deploy apply -f ployz.yaml --namespace prod --strategy rolling
```

Expected plan:

```text
1. start candidate api@rev44 on node-b
2. wait candidate readiness
3. add candidate to route
4. drain old api@rev43 on node-a
5. cleanup old api@rev43
6. repeat for next slot
```

Implementation implications:

- Rolling deploy is a deploy compiler strategy, not a background controller.
- Each step records evidence before moving to the next traffic-affecting step.
- Cleanup failure after route promotion reports follow-up work without
  pretending the deploy fully failed.

### 4. PR Branch With Fresh Runtime State

```text
ployz branch preview pr-218 --from prod -f ployz.yaml
ployz branch create pr-218 --from prod -f ployz.yaml
```

Expected behavior:

- Create namespace `pr-218`.
- Resolve services from the PR file.
- Use fresh volumes for resources marked `fresh`.
- Allocate preview routes.
- Start services and commit branch lineage.

Implementation implications:

- Branch is a front-end over the deploy compiler.
- Branch rows should reference source namespace, git metadata, resource source
  policy, and deploy commits. They should not duplicate the whole cluster view.

### 5. PR Branch With Volume Clone

```text
ployz branch create pr-219 --from prod -f ployz.yaml \
  --volume data=clone:prod/data
```

Expected behavior:

- Snapshot source volume at a known source watermark.
- Create target volume `pr-219/data` from that snapshot.
- Record clone lineage.
- Start branch services against the target volume.
- Source and branch writes diverge after clone.

Implementation implications:

- This is a volume fork plus deploy apply, not a move.
- ZFS clone is the first implementation target.
- Cross-machine clone is a later send/receive copy path with separate
  evidence.
- Data inheritance must be explicit in the plan.

### 6. Future PR From Multiple Source Branches

This is the desired long-term shape, not required for the first 1.0 branch
slice. It should wait until single-source branch, promotion, rollback, and
lineage evidence are boring.

```text
ployz branch create pr-230 \
  --from prod \
  --source api=branch:pr-210/api \
  --source web=branch:pr-227/web \
  --volume data=clone:prod/data \
  -f ployz.yaml
```

Expected behavior:

- The branch compiler resolves each resource source independently.
- Preview shows mixed source lineage:
  - `api` from `pr-210`;
  - `web` from `pr-227`;
  - `data` cloned from `prod`;
  - omitted or fresh resources are explicit.

Implementation implications:

- Do not encode "branch type" as a giant mode. Use per-resource source policy.
- Promotion must know which committed sources were composed into the target.

### 7. Promote PR To Production

```text
ployz promote preview pr-219 --to prod
ployz promote apply pr-219 --to prod
```

Expected behavior:

- Compare branch commit with current production.
- Show service revision changes, route switch, volume lineage, and rollback
  point.
- Apply production deploy using the same phase/checkpoint discipline as any
  deploy.

Implementation implications:

- Promote is not copying branch rows over production rows.
- Promote compiles a production deploy from branch lineage and current prod
  truth.

### 8. Rollback

```text
ployz rollback preview prod --to dep_122
ployz rollback apply prod --to dep_122
```

Expected behavior:

- Restore services, routes, and volume lineage to a committed deploy point.
- Show any data-loss or irreversible cleanup caveats before apply.

Implementation implications:

- Deploy commits need enough immutable data to reconstruct previous state.
- Rollback is another deploy compiler front-end.

### 9. Volume Move

```text
ployz volume move preview prod/data --to node-c
ployz volume move apply prod/data --to node-c
```

Expected behavior:

- Stop or drain writers.
- Snapshot source.
- Send to target.
- Apply final delta.
- Verify target.
- Commit owner change.
- Cleanup source after commit or report follow-up.

Implementation implications:

- The current owner machine is the transfer fence. The coordinator RPCs to the
  owner, and the owner serializes stop-writes, snapshot, and source watermark
  updates before writing Corrosion.
- Add a separate distributed claim only if a later multi-owner transfer path
  proves owner-machine serialization is not enough.

### 10. Machine Drain And Remove

```text
ployz machine drain preview node-a --all
ployz machine drain apply node-a --all
ployz machine remove preview node-a
ployz machine remove apply node-a
```

Expected behavior:

- Drain compiles service moves, volume moves, and route changes.
- Remove tombstones only after no active service/volume placement remains.

Implementation implications:

- Machine lifecycle is product behavior in Ployz, implemented through Polis
  store/RPC primitives.
- The store should not infer liveness into durable machine truth.

## What To Copy From uncloud

Useful patterns:

- A thin root CLI with global connection/context flags.
- Command functions that read current state, plan, print, confirm, and execute.
- Product specs with local defaulting and validation.
- A deploy object that owns `Validate`, `Plan`, and `Run`.
- Operations as small typed units with `Execute` and `Format`.
- Direct scheduler inputs: machines, volumes, services, and current instances.
- A thin Corrosion client with `exec`, `query`, and `subscribe`, not a generic
  persistence framework.

Things not to copy:

- Docker-only assumptions in core product models.
- Corrosion membership as the network model. Ployz uses iroh identity/RPC and
  authority island WireGuard edges for 1.0.
- JSON blobs for independently changing replicated cluster truth. JSON is fine
  for local runtime caches or opaque provider metadata, not for the row shapes
  ordinary deploy/routing/volume code queries.
- Controllers that quietly converge product policy behind the operator's back.

## Acceptance Criteria

- A new contributor can read this document and predict the public command
  shape for Ployz 1.0.
- Each command has a clear implementation owner: CLI parser, Ployz product
  module, Ployz Polis adapter, Polis substrate primitive, daemon/runtime
  backend, or presentation only.
- The roadmap can point every 1.0 implementation slice back to a concrete CLI
  workflow.
- The surface is explicit enough for cloud and coding agents to drive without
  private semantics.
