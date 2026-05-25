---
title: Ployz 1.0 Roadmap
status: draft
created: 2026-05-24
origin:
  - VISION.md
  - docs/plans/2026-05-24-002-feat-ployz-1-0-cli-shape-and-workflows-plan.md
  - docs/plans/2026-05-24-003-feat-ployz-1-0-state-and-substrate-plan.md
  - docs/plans/2026-05-24-004-feat-ployz-1-0-deploy-branch-volume-plan.md
---

# Ployz 1.0 Roadmap

## 1.0 Definition

Ployz 1.0 is a small-cluster orchestration core that can run real production
workloads through explicit operator commands:

- machines join, leave, drain, and expose clear diagnostics;
- namespaces define deployment/resource scope;
- services deploy through previewable plans;
- rolling deploys promote traffic only after readiness;
- volumes can be created, forked, moved, and rolled back through ZFS-backed
  evidence;
- PR branches can run with fresh or cloned state;
- promote and rollback are first-class deploy compiler front-ends;
- cloud and agents can drive the same public CLI/API without private semantics.

The product should stay closer to `~/dev/uncloud` simplicity than the old
legacy codebase:

- direct commands;
- typed product specs;
- explicit plan/confirm/execute flows;
- small operation structs;
- thin Corrosion access;
- no hidden desired-state controller;
- no generic substrate framework invented before it is needed.

## Non-Negotiable Architecture

- `polis` owns substrate primitives: Corrosion rows, transactions,
  subscriptions, iroh identity, tickets, peer RPC, probes, deadlines, and
  distributed failure typing.
- `ployz` owns product behavior: machine lifecycle, namespace meaning, deploy
  semantics, branching, routing, volume movement, readiness, placement, and
  operation outcomes.
- Ployz adapters translate between product ports and Polis primitives.
- Corrosion stores row-shaped cluster state. It is not the command bus.
- iroh RPC carries bounded peer commands.
- WireGuard peers are the machines in the same authority island for 1.0.
  Dynamic namespace-scoped networking is a post-1.0 optimization to reduce mesh
  scope for large islands.
- Post-1.0, separate authority islands communicate by RPC. A laptop can be its
  own island and ask a production island to deploy resources; the production
  island authorizes and writes its own rows.
- Tickets are bootstrap envelopes. Durable identity is iroh endpoint ID.
- Owner-machine serialization is the default fence. Coordinators RPC to the
  resource owner, and that owner enforces local ordering before writing its
  Corrosion rows. Explicit distributed claims are a later escape hatch for a
  proven multi-owner path.

## Roadmap Tracks

### Track A: CLI And Public Contract

Goal: make the target product surface concrete before implementation spreads.

Deliverables:

- root CLI crate/binary;
- global connection/context handling;
- command tree from the CLI plan;
- human and JSON output envelopes;
- preview/apply/verify conventions;
- exit-code contract;
- public API structs shared by CLI/cloud/agents.

First slices:

1. `ployz status` and `ployz doctor` over local daemon/substrate diagnostics.
2. `ployz machine list/inspect`.
3. `ployz deploy preview` rendering a plan from an in-memory fixture.
4. `ployz deploy apply --yes` calling the product engine.

Done when:

- every planned 1.0 workflow has a visible command;
- JSON output can be consumed by a zero-context agent without parsing human
  text;
- command help text names the risk and confirmation behavior for mutating
  commands.

### Track B: Substrate Spine

Goal: two daemons can discover each other, run RPC, sync Corrosion rows, and
survive restart with stable identity.

Deliverables:

- local iroh key load/create;
- endpoint and RPC server lifecycle;
- bootstrap ticket create/join;
- Corrosion process lifecycle;
- schema apply;
- store transaction/query/subscription primitives;
- machine row upsert/observe;
- two-node membership e2e.

First slices:

1. Finish Corrosion store primitive over `corro-client`.
2. Add iroh endpoint/RPC smoke test without Corrosion.
3. Add machine membership vertical slice.
4. Add restart identity/adoption test.

Done when:

- a returning machine keeps the same endpoint ID;
- `machine add` writes/observes rows through Corrosion;
- peer RPC has explicit deadlines and typed failures.

### Track C: Authority Island Mesh And Namespaces

Goal: network every machine in the authority island together, while keeping
namespace as a deploy/resource grouping rather than a network boundary.

Deliverables:

- `namespaces` table;
- product namespace model;
- authority island peer query;
- local WireGuard controller/adoption;
- namespace diagnostics.

First slices:

1. Add namespace rows.
2. Derive full authority island peer set for one machine.
3. Rebuild WireGuard config from derived peers.
4. Expose namespace inspection and `doctor mesh`.

Done when:

- machines in the same authority island get network edges;
- namespace changes do not rewrite WireGuard policy;
- a daemon restart rebuilds the same mesh without rewriting cluster truth.

### Track D: Runtime Backend

Goal: deploy operations can start, verify, stop, inspect, and clean up
workloads on target machines.

Deliverables:

- internal runtime RPC protocol;
- runtime backend contract;
- local container backend;
- health/readiness checks;
- logs/exec basics;
- adoption of already-running instances.

First slices:

1. Start/stop/inspect a trivial workload on one machine.
2. Add readiness check with timeout.
3. Add logs surface for failed deploy phases.
4. Add restart adoption.

Done when:

- deploy phases do not deserialize public CLI requests over peer RPC;
- runtime errors are typed by caller action: retry, repair, unsupported, or
  peer unavailable.

### Track E: Single-Service Deploy MVP

Goal: deploy one HTTP service to one namespace with durable evidence.

Deliverables:

- deploy manifest model;
- planning state;
- typed deploy operations;
- image availability/distribution primitive;
- service revision rows;
- service instance placement rows;
- service instance observation rows;
- route rows;
- deploy phase rows;
- deploy commit rows;
- gateway/DNS projection from committed rows.

First slices:

1. Manifest validation/defaults.
2. Pure planning test for one service.
3. Runtime apply on one machine.
4. Route commit/projection.
5. `deploy history` and `deploy verify`.

Done when:

- `ployz deploy preview/apply/verify` works for one service;
- failed readiness does not promote route;
- deploy history can reconstruct what was attempted and what committed.

### Track F: Rolling Deploys

Goal: replace running instances without losing the old route until the
candidate is ready.

Deliverables:

- rollout policy model;
- candidate machine selection;
- start-first/stop-first decision;
- route step checkpoints;
- old instance drain and cleanup retry surface;
- canary/one-at-a-time tests.

First slices:

1. Plan no-op/create/replace/remove operations.
2. Implement start-first for stateless services.
3. Implement stop-first for port conflict and single-writer volume cases.
4. Add cleanup follow-up result.

Done when:

- rolling deploy can tolerate a candidate readiness failure before route
  switch;
- failed cleanup after route switch reports exact recovery command.

### Track G: ZFS Volumes

Goal: make stateful operations a first-class product primitive.

Deliverables:

- volume rows;
- snapshot rows;
- ZFS backend contract;
- fresh volume create;
- snapshot;
- fork/clone;
- send/receive move;
- volume rollback support.

First slices:

1. ZFS create/snapshot/clone local test.
2. `volume create`.
3. `volume fork` same-machine clone.
4. Deploy with fresh/forked volume.
5. `volume move` send/receive with final delta.

Done when:

- a PR branch can clone prod data explicitly;
- source and target writes diverge after fork;
- move preserves volume identity and changes owner only after verification.

### Track H: Branch, Promote, Rollback

Goal: PR and branch workflows compile into the same deploy discipline as
production deploys.

Deliverables:

- branch namespace lifecycle;
- per-resource source policy;
- branch lineage;
- multi-source branch composition;
- promotion compiler;
- rollback compiler;
- branch delete cleanup.

First slices:

1. Branch fresh mode.
2. Branch with volume clone.
3. Multi-source branch preview.
4. Promote stateless branch.
5. Rollback stateless deploy.
6. Stateful promote/rollback once volume evidence is sufficient.

Done when:

- PR branches can be created, updated, inspected, promoted, and deleted;
- branch source lineage is visible and durable;
- rollback can state exactly what is reversible and what is not.

### Track I: Machine Drain And Removal

Goal: machine lifecycle is safe because drain compiles the exact product work.

Deliverables:

- drain compiler;
- workload replacement plan;
- volume movement plan;
- route migration plan;
- remove preflight;
- tombstone semantics.

First slices:

1. Drain preview for stateless services.
2. Drain apply for stateless services.
3. Drain with volume move.
4. Machine remove after empty.

Done when:

- remove refuses active placements;
- drain lists every affected service, volume, route, and follow-up task.

## Execution Order

### Milestone 0: Keep Current Corrosion/Iroh Slice Honest

- Completed by the substrate-spine e2e slice: real Corrosion lifecycle/schema,
  real iroh peer preflight, Corrosion-backed machine add, two-node row
  visibility, and restart-stable endpoint identity.
- Old p2panda/NATS/fact-store guidance is historical when it conflicts with
  the current Corrosion + iroh substrate direction.
- Machine row comments document row ownership and why the current machine
  `epoch` is only an owner-issued row version, not a global conflict solution.

### Milestone 1: CLI Skeleton And Substrate Smoke

- Add CLI crate and root command.
- Add `status`, `doctor`, `machine list`, `machine inspect`.
- Add local daemon substrate startup: `ployzd` composes the Polis Corrosion
  agent, applies membership schema, starts persistent iroh identity, reports
  typed substrate startup state, and shuts down cleanly.
- Reuse the substrate-spine e2e as the daemon startup regression target.

### Milestone 2: Authority Island Mesh And Namespaces

- Add namespace rows.
- Derive full authority island WireGuard peers.
- Expose namespace commands.
- Add mesh diagnostics.

### Milestone 3: Single-Service Deploy

- Add manifest model.
- Add pure planner.
- Add runtime RPC start/verify/stop.
- Add service revision/instance/route/deploy evidence rows.
- Ship one-service `deploy preview/apply/verify`.

### Milestone 4: Rolling Deploy

- Add rollout policy.
- Add replace/remove planning.
- Add route checkpoints.
- Add cleanup follow-up state.

### Milestone 5: Volumes

- Add ZFS backend.
- Add volume rows and fresh create.
- Add snapshot/fork.
- Add deploy with volume attach.
- Add volume move.

### Milestone 6: Branch Workflows

- Add branch namespace lifecycle.
- Add per-resource source policy.
- Add branch create/update/delete.
- Add PR-oriented JSON output.
- Add multi-source branch composition.

### Milestone 7: Promote, Rollback, Drain

- Add promotion compiler.
- Add rollback compiler.
- Add stateless then stateful rollback support.
- Add machine drain/remove.

### Milestone 8: Hardening For 1.0

- Full e2e matrix across two or more nodes.
- Crash/restart tests during every deploy checkpoint class.
- Corrosion subscription resume tests.
- RPC deadline/failure tests.
- ZFS cleanup/recovery tests.
- CLI JSON compatibility tests.
- Docs: operator guide, failure guide, branch/volume guide, architecture guide.

## Simplicity Checks Before Each Slice

Ask these before implementing:

- Can this be a typed operation plus direct apply, like uncloud, rather than a
  controller?
- Does this row have one obvious owner?
- Is JSON only used for opaque metadata, not a set/map with independent
  writers?
- Is this a Ployz product concept that should stay out of Polis?
- Can the preview prove the dangerous part before mutation?
- Does failure name the audience and next action?
- Can a daemon restart adopt the last good state?
- Can the next slice be useful without finishing all future slices?

## 1.0 Release Gates

- CLI workflows documented in the CLI plan work in e2e tests.
- Two-node cluster passes public machine add, deploy, rolling deploy, branch,
  promote, rollback, volume fork, volume move, drain, and remove tests.
- Every external control-plane I/O path has a deadline.
- Every mutating command has human and JSON output.
- Corrosion schema changes are additive and file-backed.
- No ordinary Ployz module imports Corrosion, iroh, or irpc types.
- No hidden background task rewrites product truth.
- Failed-after-checkpoint states are visible and recoverable.
