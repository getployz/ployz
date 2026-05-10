# Deploy Primitives Roadmap

## Thesis

Deploy should become the compiler for explicit infrastructure operations.

An operator, CLI command, cloud workflow, or agent should be able to express a
high-level operation such as "branch this namespace", "move this service",
"clone this volume", "prepare production from this branch", "promote this
branch", or "drain this machine". Ployz core then resolves that intent into a
typed, previewable, phase-aware deploy plan with bounded participants, visible
preflights, explicit commit boundaries, and durable evidence.

This keeps the cluster aligned with `VISION.md`: primitives instead of hidden
policy, foreground operations instead of background reconciliation, and live
decisions instead of a standing desired-state controller.

## Why Deploy Is The Compiler

Most useful infrastructure operations touch the same things:

- service revisions and placement,
- volume identity, lineage, and ownership,
- runtime instances and their readiness,
- routing facts,
- machine reachability and participation,
- commit and rollback boundaries.

If every command implements its own mutation path, correctness fragments. A
volume move, PR branch, promotion, machine drain, and production redeploy all
need the same discipline: resolve current truth, preview the work, prove
participants, run preflights, mutate in the right order, commit at the right
point, and report exactly what happened.

Deploy is the right core primitive because it already owns that sequence. Higher
level commands should compile into deploy manifests, deploy intent, or
deploy-adjacent operation requests that produce the same plan and evidence
surface.

## Core Vocabulary

### Fresh

Create a new resource with no inherited runtime state.

Examples:

- new service revision in a namespace,
- fresh managed volume,
- fresh PR database.

Fresh work has no source lineage. Preview should show creation and placement,
but not branch or clone evidence.

### Branch

Create a new target identity from a committed source identity.

Examples:

- service `pr-39/web` from committed `prod/web`,
- volume `pr-39/data` from a snapshot clone of `prod/data`.

Branch work creates lineage. The target is independent after commit. Preview and
commit evidence must name the source namespace, source resource, source
revision or snapshot, target identity, and participants.

### Portal

Attach to an existing resource from another namespace without copying it.

Examples:

- PR web service calls the production backend,
- PR web/backend uses production Postgres read-only,
- staging service routes to a shared internal dependency.

Portal is not branch. It is an explicit opt-in attach/reference primitive. The
plan must make that explicit so operators do not confuse "using prod" with
"branched from prod". Portal capabilities need stricter safety rules than clone
because the target operation can affect or depend on live source state.

Portal must be denied by default until source namespace/resource policy
explicitly permits the target namespace and mode. Preview and apply must enforce
the same visibility boundary so portal preview cannot become a cross-namespace
enumeration API.

### Move

Preserve identity while changing placement or ownership.

Examples:

- move volume `prod/data` from `machine-a` to `machine-b`,
- move all services attached to that volume,
- move workloads off a draining machine.

Move work needs writer quiescing, transfer evidence, target startup, durable
ownership commit, and cleanup of old placement. It should move attached services
when the service cannot safely remain on the old machine.

### Migrate

Run a composed operation that may include moves, restarts, data transfer, and
validation under one user-facing command.

Examples:

- `ployzctl migrate service db --to machine-b`,
- `ployzctl machine remove machine-a`,
- "disk is low; move these volumes and their writers".

Migrate is the user-facing primitive. Deploy phases are the execution model.

### Promote

Make prepared branch state become production state.

`promote` remains the north-star primitive from `VISION.md`: an atomic
production traffic switch with a clear rollback point. The near-term precursor
is not yet that primitive. It is "prepare production from branch lineage": a
normal production deploy that adopts committed branch service/volume lineage and
shows exactly what will change.

The product should not call that precursor `promote` until traffic switch,
rollback point, routing evidence, and state-lineage rules are strong enough to
honor the atomic promotion promise.

### Checkpoint

Commit part of a deploy before later phases run.

Examples:

- database migration completed and committed before web rollout,
- volume ownership changed before dependent services start,
- branch data cloned before app services attach.

A checkpoint is a point of no return for its committed facts. Later failure must
report "failed after checkpoint" instead of implying the whole operation rolled
back.

## Operation Flow

Every compiled operation should follow the same shape:

1. **Resolve.** Read stored intent and live observations needed for the
   decision.
2. **Preview.** Produce a typed plan with phases, participants, work,
   preflights, warnings, commit policies, rollback policies, and lineage.
3. **Lock.** Acquire the target namespace or operation lease before mutation.
   If the operation depends on source namespace truth, also acquire the
   relevant source resource lease or use an explicit source revision
   compare-and-swap.
4. **Revalidate.** Re-resolve the plan and reject drift before participant RPCs
   when the operation depends on stable source truth.
5. **Preflight.** Prove reachability, placement, runtime candidates, writers,
   storage capabilities, and rollback/checkpoint conditions.
6. **Execute.** Run phase work in order against explicit participants.
7. **Commit.** Append durable deploy commits at phase checkpoints and final
   deploy boundaries.
8. **Publish.** Emit routing or runtime projection events derived from durable
   truth.
9. **Cleanup.** Remove old instances or provisional artifacts when safe.
10. **Report.** Return structured events, phase states, warnings, residual
    cleanup status, and retry guidance.

The important property is not that every operation is literally encoded in a
single `DeployManifest`. The important property is that every operation uses the
same typed plan, preflight, commit, and evidence discipline.

## Core Owns vs Cloud Owns

Core owns semantics:

- deploy manifest and intent validation,
- service and volume source modes,
- placement and participant selection,
- phase planning and execution,
- preflight definitions,
- checkpoint and rollback semantics,
- lineage and movement evidence,
- structured errors and retry boundaries,
- SDK/API schema.

Cloud owns experience:

- repository and PR event handling,
- UI choices such as "fresh database" vs "clone prod database",
- Inngest or job orchestration that calls core operations,
- deploy queue presentation,
- billing, teams, auth, notifications,
- storing user workflow state and cached cluster views.

Cloud may compile UI choices into core manifests or operation requests. It must
not invent private cluster semantics that the CLI and SDK cannot express.

## Branching Model

The eventual branch operation should compile a source namespace and target
namespace into independent target resources by default:

| Mode | Meaning | Example |
| --- | --- | --- |
| Fresh | Create independent new state | PR Postgres starts empty |
| Branch | Create independent state from source lineage | PR Postgres is a ZFS clone of prod |

The target branch should be inspectable before apply:

- which services are fresh or branched,
- which volumes are fresh or cloned,
- which machines participate,
- which source revisions or volume snapshots are used,
- which preflights may stop or reject live work,
- which facts commit before app startup.

This lets cloud offer friendly presets without hiding the core primitive:

- "fresh PR environment",
- "PR environment with cloned database".

## Portal And Attach Model

Portal belongs next to branch, not inside the default meaning of branch. It is a
separate attach/reference primitive for the cases where a target namespace
intentionally depends on source namespace state.

Before portal becomes executable, the model needs:

- source namespace/resource opt-in policy,
- target namespace authorization,
- preview redaction or rejection for unauthorized callers,
- mode-specific safety rules such as read-only, service-only, or volume
  read-only,
- explicit evidence that the target is attached to live source state, not forked
  state,
- cleanup and revocation behavior.

The first safe portal mode is still an open decision. Until that decision is
made, portal should remain rejected or reserved, not partially previewable in a
way that leaks source topology.

## Move And Migration Model

Volume and service movement should be deploy-phase work, usually generated by
commands:

- `volume move`: move a volume and any services that must move with it,
- `migrate service`: move service runtime and attached single-scope state,
- `machine drain/remove`: generate moves for workloads and state on a machine.

The plan should make attached service movement explicit. If a single-scope
volume moves, services that mount it cannot be planned on the old machine unless
the mount is portal/read-only/shared in a way the model proves safe.

A minimum-downtime move should eventually compile into phases:

1. start transfer from source to target,
2. stop or drain writers,
3. transfer final delta,
4. start attached services on target,
5. commit new volume ownership and service slots,
6. cleanup old placement.

## Production Adoption And Promotion Model

Near-term production adoption should start conservative:

- prepared branch state exists separately,
- production deploy references that state as source lineage,
- production preview shows exactly what will be adopted,
- production apply commits normal deploy evidence.

This is a non-atomic production adoption MVP, not the final `promote`
primitive. It differs from atomic promotion in observable ways: production goes
through the normal deploy lifecycle, rollback is whatever that deploy can still
reverse, and there is no single namespace traffic-switch commit that preserves a
previous branch as the rollback target.

The final promotion primitive should be an atomic namespace traffic switch with
its own readiness checks, rollback point, routing evidence, state adoption
semantics, and volume lineage rules.

## Phasing, Checkpoints, And Rollback

Deploy phases are the unit that keeps operation semantics honest.

Phase policy should answer:

- When does this phase run?
- What work does it own?
- Which participants are required?
- Does success commit durable truth immediately?
- Can rollback undo it?
- If rollback cannot undo it, what does failure after this phase mean?

Examples:

- DB upgrade phase: `commit_policy = Checkpoint`,
  `rollback_policy = ForwardOnly` or `External`.
- Web rollout phase: `commit_policy = EndOfDeploy`,
  `rollback_policy = Reversible`.
- Volume move phase: checkpoint only after transfer evidence proves the target
  can own the volume.
- Portal validation phase: may have no store commit, but still returns
  structured evidence or rejection.

Recurring or tiered rollout should be command-shaped. Cloud can schedule
`deploy resume` via Inngest. Core should not wake itself in a background loop to
advance rollout policy.

Early phase support should be generated by known primitives only. Operators
should not be able to author arbitrary workflow graphs, arbitrary resume
policies, or unbounded phase DSLs in the deploy manifest. When a workflow shape
recurs often enough, it should become a named primitive.

## Production Data Policy

Branching and portal-like workflows can expose production-derived state. That is
a product and security boundary, not a UI detail.

Before cloud presets advertise "clone prod database" or "attach to prod", core
and cloud need explicit policy for:

- which data classes may be cloned or referenced,
- whether masking/anonymization is required,
- whether credentials are read-only or scoped,
- how branch cleanup and retention work,
- which identifiers may appear in preview/apply evidence,
- how logs avoid credentials or sensitive payloads,
- whether the source namespace/resource must opt in.

The v1 core primitive can support raw clone mechanics, but managed cloud
workflows should not imply production-data safety until these policies are
modeled.

## Evidence Contract

Preview evidence is for planned work. Apply evidence is for work that actually
ran.

Preview should include:

- service source modes,
- volume source modes,
- phases and phase work,
- participants,
- preflight scopes,
- commit and rollback policies,
- lineage or movement plans,
- warnings for degraded or uncertain conditions.

Apply should include:

- preflight events,
- participant RPCs that matter to the operator,
- phase start/finish/commit events,
- source snapshot or transfer proof,
- started/removed/drained runtime instances,
- cleanup failures and recovery status,
- final state and rollback limits.

Warnings should not be used for normal planned work. A clone replacement
preflight is planned work, so it belongs in typed preview evidence. A missing
participant or unsupported policy belongs in structured rejection.

## API And Generation Direction

The public schema should be generated from the Rust model where practical and
consumed by cloud/dashboard clients.

The target shape:

- Rust core defines request, preview, apply, event, and error models.
- JSON schema/OpenAPI/TypeScript are generated from those models.
- Dashboard imports generated clients/types instead of shadowing deploy
  semantics.
- Cloud jobs call core operations and persist cloud workflow state, not cluster
  truth.
- Generated schemas and docs carry support-status and security semantics for
  source modes: unsupported/reserved values, authorization requirements,
  redaction behavior, and apply rejection guarantees.
- New public preview fields are not a cloud contract until schema generation
  exports them. Before that, they are core-internal evidence.

This is the only way the dashboard stays a lens over the core instead of a
second orchestration system.

## Near-Term Slices

1. **Service branch source preview.** Add typed preview evidence for fresh
   derived services and branch-derived services. Keep portal rejected/reserved
   until safe attach semantics are chosen.
2. **Branch command skeleton.** Add a command that renders a branch deploy from
   source namespace, target namespace, and per-resource source modes.
3. **Volume/service migrate plan generation.** Make move/migrate commands
   generate phase work with attached-service movement instead of bespoke
   mutation paths.
4. **Checkpoint policy hardening.** Strengthen phase commit and rollback policy
   around forward-only database/volume work.
5. **Generated API package.** Establish Rust-to-schema-to-TypeScript generation
   for deploy preview/apply models so cloud can consume core semantics directly.

## Non-Goals

- No general-purpose workflow engine in core.
- No cloud-only operation semantics.
- No background reconciler that advances deploy policy on its own.
- No hidden mutation in preview.
- No "best effort" success when a committed checkpoint or cleanup residual
  limits rollback.
- No generic storage abstraction that hides whether clone/move/rollback is
  actually supported.

## Open Questions

- What is the first safe portal mode: service-to-service only, volume read-only,
  or both?
- Should service branch lineage copy the exact committed source spec, reference
  a revision hash, or both?
- Where should generated TypeScript live long term: this repo, dashboard repo,
  or a published SDK package?
- Which promotion mode should ship first: redeploy production from branch
  lineage as a precursor, or the final explicit routing switch?
- What is the smallest rollback primitive that makes volume move checkpoints
  understandable to operators?
