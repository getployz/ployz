---
title: Functional System Roadmap
status: draft
created: 2026-05-24
---

# Functional System Roadmap

This catalog lists the work required to get from the current Corrosion + iroh
substrate slice to a fully functional Ployz core: machines, deploys, rolling
deploys, branching, promotion, rollback, volumes, routing, and local dev.

The execution roadmap is `docs/architecture/ployz-1-0-roadmap.md`. The CLI
contract starts in
`docs/plans/2026-05-24-002-feat-ployz-1-0-cli-shape-and-workflows-plan.md`.
This document remains a functional catalog for checking that the roadmap does
not drop a required product capability.

The goal is feature parity with the useful legacy capabilities, not parity with
legacy complexity. The roadmap follows `VISION.md`: explicit foreground
commands, visible preconditions, bounded effects, structured failures, and no
hidden reconcilers.

## Complexity Cuts

Do not bring these back unless a product path proves they are needed:

- background controllers that silently rewrite durable truth;
- a generic store framework over Corrosion;
- product-shaped Polis APIs such as `machines.join`, `deploy.record_ready`, or
  `capacity.reserve`;
- a generic operations table for every command before command evidence needs
  durable replay;
- global locks by default;
- durable iroh tickets as machine truth;
- opaque JSON blobs for independently changing cluster state;
- scripting multiple low-level commands where a first-class primitive should
  exist.

Preferred shape:

- Ployz modules own product behavior.
- Ployz adapters sequence Polis primitives.
- Polis owns Corrosion, iroh, tickets, peer RPC, subscriptions, deadlines, and
  substrate failure typing.
- Most rows have one clear owner and are written idempotently. Resource-owned
  rows are written by the machine that owns the resource; coordinators RPC to
  that owner rather than writing the row from another machine.
- Owner-machine serialization replaces most explicit claims/fences. If the
  machine that owns a resource row is the only writer, it can enforce the
  ordering locally before writing Corrosion. Add a separate distributed
  claim/fence only when a concrete multi-owner path proves owner serialization
  is insufficient.

## Required Product Concepts

### Machine And Mesh

- Machine identity: `machine_id`, `island_id`, iroh endpoint ID, WireGuard
  public key, overlay IP, capabilities, lifecycle.
- Public machine add: load identity, start iroh, preflight peer RPC, insert
  machine row, derive local runtime configuration. "Join" is the internal
  ticket/bootstrap step.
- Machine update: owner-written capability/network updates with conflict
  visibility.
- Machine drain: mark no-new-placement, plan workload and volume movement,
  commit drain result, keep cleanup visible.
- Machine remove: drain first, remove mesh edges, tombstone machine row, leave
  auditable removal evidence.
- Authority island mesh: V1 networks every machine in the same authority
  island together. Dynamic namespace-scoped WireGuard is a post-1.0
  optimization to reduce mesh scope for large islands, not the V1 network
  model.
- Post-1.0 authority islands: a laptop can be its own authority island and RPC
  into another authority island to ask that island to deploy resources. The
  destination island authorizes and writes its own rows.
- Data-plane adoption: daemon restart adopts existing WireGuard, Corrosion,
  gateway, DNS, and workloads without restarting last-good service.

### Namespaces And Environments

- Namespace identity and lifecycle: production, staging, branch, local dev.
- Namespace meaning: a deploy/resource grouping. A deploy looks at what exists
  in the target namespace from the reached node's perspective, computes the
  diff, and runs that foreground operation.
- Environment source modes:
  - fresh: no inherited runtime state;
  - branch: independent clone from committed source lineage;
  - portal: explicit live attach, denied until safety policy exists.
- Namespace deletion: explicit drain and cleanup, not hidden garbage
  collection.

### Services And Images

- Service identity: service name, namespace, runtime kind, exposed ports,
  health/readiness policy, attached volumes, secrets references.
- Service revision: immutable runnable revision with image/build artifact,
  config digest, source metadata, and declared dependencies.
- Image/build pipeline:
  - local build input,
  - image inspection,
  - image availability per machine,
  - explicit distribution to target machines,
  - transfer evidence.
- Runtime placement rows: namespace, service, revision, machine, lifecycle, and
  cleanup state. Runtime observations such as readiness are separate
  owner-machine rows or fresh probe receipts.
- Runtime participant RPC: start, stop, verify, drain, cleanup, inspect logs.

### Deploy Compiler

- Deploy request model: target namespace, service changes, volume changes,
  routing intent, rollout policy, deadline, idempotency key.
- Preview model: phases, participants, preflights, warnings, commit points,
  rollback options, cleanup tasks.
- Apply model:
  1. inspect current namespace state from the reached node's perspective;
  2. probe live preconditions;
  3. ask resource owner machines to serialize their own writes;
  4. revalidate drift;
  5. run participant work;
  6. commit durable rows at checkpoints;
  7. publish routing projection events;
  8. cleanup old instances;
  9. report structured result.
- Deploy phase rows: phase state, participant, command, started/finished time,
  failure, commit linkage.
- Deploy commit rows: immutable committed service, instance, routing, lineage,
  and volume evidence.
- Idempotent replay: same command/key observes existing phase/commit and
  reports current state without redoing unsafe work.

### Rolling Deploys

- Rollout policy: all-at-once, one-at-a-time, max unavailable, canary count.
- Candidate planning: choose machines from the authority island and live
  capacity probes.
- Readiness gates: candidate instance must be reachable and pass service
  readiness before route inclusion.
- Traffic step: wait for readiness, add candidate route, drain old instance,
  then commit step evidence.
- Failure behavior: fail before commit when possible; after checkpoint, report
  failed-after-checkpoint with exact live/old instance status.
- Cleanup behavior: old instance cleanup failure is visible recoverable status,
  not deploy failure after traffic is live.

### Volumes And Storage

- Volume identity: namespace, volume id, owner machine, attached service,
  storage backend, lifecycle, current watermark.
- Snapshot identity: source volume, source watermark, backend snapshot id,
  creation evidence.
- Clone/fork: create independent target volume from source snapshot, record
  lineage, attach to target service.
- Move: stop or drain writers, snapshot, transfer, final delta, receive,
  verify, commit new owner, cleanup source artifact.
- Rollback: restore service and volume to a prior committed deploy point.
- Storage capability model: ZFS primary for 1.0. Non-ZFS backends, including a
  possible Btrfs small-machine tier, return explicit unsupported failures until
  a concrete implementation slice exists.
- Volume transfer RPC: coordinators talk to the current owner machine for
  source stop-writes/snapshot/final-delta, and to the target owner for
  receive/verify/activate. Volume rows are written by the machine that owns, or
  is atomically becoming owner of, the volume.

### Routing, Domains, Gateway, DNS

- Domain/certificate readiness remains a product precondition for HTTPS
  deploys.
- Route rows are committed route intent. Routing projections are derived from
  route rows plus current runtime readiness.
- Gateway/DNS rebuild from durable route rows, then consume ordered routing
  events.
- Route promotion is an explicit commit boundary: traffic flips only after
  candidate readiness and durable commit.
- Rollback restores a previous deploy point and republishes routing projection
  events.
- Gateway/DNS never write health back into cluster truth.

### Branch, Promote, Rollback

- Branch preview: show service sources, volume sources, machines,
  preflights, snapshots, clone targets, and portal denials.
- Branch apply: clone volumes, create service revisions/instances, assign
  routes, commit branch lineage.
- Promote preview: compare branch commit to production, show route switch,
  volume lineage, and rollback point.
- Promote apply: commit production deploy point, switch routing, retain prior
  point for rollback.
- Rollback apply: restore committed service/routing/volume point with the same
  phase discipline as deploy.

### CLI, API, SDK, And Agent Surface

- External commands:
  - `machine add`
  - `machine remove`
  - `deploy`
  - `migrate`
  - `branch`
  - `promote`
  - `rollback`
  - `volume fork`
  - `dev`
- Every command returns structured JSON-friendly output: plan id, phases,
  committed rows, warnings, failures, retry guidance.
- Every command has `preview`, `apply`, and `verify` surfaces when meaningful.
- Errors are typed by audience: caller retry, operator repair, peer failure,
  substrate unavailable, unsafe precondition.

## Roadmap Order

### 1. Finish The Substrate Spine

- Status: local test/e2e proof exists for Corrosion lifecycle, schema load,
  two-node row visibility, and restart-stable iroh identity. The next daemon
  slice makes `ployzd` compose the Polis Corrosion agent, apply schema, start
  persistent iroh identity, report typed substrate startup state, and shut down
  cleanly.
- Store transactions, queries, subscriptions, and updates exposed through
  narrow Polis primitives.
- Iroh identity, ticket import/export, peer RPC listener/client, probe
  deadlines.
- Machine row schema and machine membership adapter over Corrosion.
- Corrosion-backed machine e2e proving two nodes can join and observe rows.

### 2. Machine And Mesh Runtime

- Authority island WireGuard full mesh for all machines in the island.
- Runtime adoption for WireGuard, Corrosion, gateway, DNS.
- Machine capabilities and live capacity probes.
- Machine update and remove/drain product ports.
- Diagnostics for endpoint reachability, row visibility, mesh path, and
  authority island participation.

### 3. Single-Service Deploy MVP

- Service/revision/instance/routing rows.
- Runtime participant RPC for start, verify, stop, cleanup.
- Image inspect and explicit image distribution to one machine.
- Deploy preview and apply for one HTTPS service on one machine.
- Gateway/DNS projection from committed route rows.

### 4. Deploy Phases And Durable Evidence

- Deploy phase rows and immutable deploy commit rows.
- Checkpoint semantics and failed-after-checkpoint result.
- Idempotent replay by command id/key.
- Cleanup state that can be retried independently.
- Operator-visible deploy history and verify command.

### 5. Volume Base

- Volume rows, snapshot rows, clone lineage rows.
- ZFS create/snapshot/clone backend contract.
- Fresh volume attach to service deploy.
- Fork-volume primitive for copy-on-write clone.
- Volume rollback to committed snapshot.

### 6. Branching

- Namespace rows.
- Branch preview from source namespace to target namespace.
- Branch apply for services and volumes using fresh or clone source modes.
- Branch route allocation and DNS/gateway projection.
- Branch cleanup/delete command.

### 7. Rolling Deploys

- Multi-instance service model.
- Rollout policy in deploy request.
- Candidate readiness gates and traffic step commits.
- Old instance drain and cleanup retry surface.
- Canary and one-at-a-time rollout tests.

### 8. Promotion And Rollback

- Promotion preview and apply from branch to production.
- Atomic routing switch backed by committed deploy point.
- Previous deploy point retention.
- Rollback of services, routes, and volume lineage.
- Verify command for promoted or rolled-back state.

### 9. Migration And Machine Removal

- Volume move with writer drain and final delta.
- Service migrate to target machine.
- Machine drain plan that compiles service and volume moves.
- Machine remove that tombstones only after drain verification.
- Explicit repair path for cleanup leftovers.

### 10. Local Dev And Operational Polish

- `ployz dev` using the same machine/deploy/branch primitives locally.
- Human-readable and JSON output parity.
- Structured diagnostics for Corrosion, iroh, WireGuard, runtime, gateway,
  DNS, storage, and routing.
- Documentation for operator workflows and failure repair.
- SDK/API stabilization around the primitive surface.

## Cross-Cutting Test Matrix

- Unit tests for product planners: machine, deploy, rollout, volume, branch,
  promote, rollback.
- Polis contract tests for Corrosion row writes, subscriptions, and peer RPC.
- Runtime fake tests for participant command sequencing and failure mapping.
- E2E scenarios:
  - machine add across two nodes;
  - single service deploy;
  - rolling deploy with failing candidate;
  - volume fork and rollback;
  - branch with cloned volume;
  - promote then rollback;
  - machine drain/remove with service and volume migration;
  - daemon restart adoption.
- Repeated tests for idempotency: replay same command/key after success,
  before commit failure, after checkpoint failure, and during cleanup failure.

## Design Gates

Before adding each slice, check:

- Is this a first-class primitive or hidden policy?
- Can preconditions fail before mutation?
- Which actor owns each durable row?
- Is any row multi-writer? If yes, does the primary key model that source?
- Which exact commit point makes the operation irreversible?
- What survives daemon restart?
- What does the operator see if cleanup fails?
- Is the complexity in Ployz product code or in Polis substrate mechanics?
- Can a zero-context reviewer explain the operation without reading legacy
  code?
