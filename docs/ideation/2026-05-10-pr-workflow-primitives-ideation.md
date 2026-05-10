---
date: 2026-05-10
topic: pr-workflow-primitives
focus: ultimate PR workflows enabled by deploy, branch, portal, promote, migrate, checkpoint, and related primitives
mode: repo-grounded
---

# Ideation: PR Workflow Primitives

## Grounding Context

Ployz is an explicit-command orchestration core for small clusters. `VISION.md`
names deploy, migrate, branch, promote, rollback, fork-volume, machine add, and
machine remove as product primitives. The core product bet is that operators and
agents should drive concrete foreground operations with visible preconditions,
bounded effects, clear results, and verification hooks, instead of relying on
hidden controllers or standing desired-state reconciliation.

The deploy roadmap in `docs/architecture/deploy-primitives-roadmap.md` frames
deploy as the compiler for higher-level operations. Branching, portal
references, volume clones, movement, promotion, drain, and production adoption
should compile into typed, previewable, phase-aware deploy plans with explicit
participants, preflights, commit boundaries, rollback policy, and durable
evidence. Cloud may generate those plans from PR events and UI choices, but core
owns the operation semantics.

The current branch-source plan in
`docs/plans/2026-05-10-006-feat-service-source-primitives.md` narrows the next
service-side slice: preview should distinguish fresh-derived and branch-derived
services, keep portal rejected until safe attach semantics exist, and keep
service move rejected until a dedicated migration primitive exists. That
matters for PR workflows because the preview contract is becoming the typed
language cloud and CLI will consume.

`docs/future/cloud.md` already sketches the commercial end state: PR
environments can be created from canvases, ZFS snapshot clones make stateful
preview environments cheap, data masking can run after clone and before app
startup, warm pools can pre-create expensive phases, PR updates should deploy
deltas only, and PR close should run destroy hooks and tear down cloned
datasets.

Relevant documented learnings:

- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`
  says final participant sets, compatibility, and eligibility must be proven
  before mutation.
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`
  says status surfaces must distinguish stored truth from live observation.
- `docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md`
  shows the value of compiling lifecycle intent into explicit previewable deploy
  work without creating a reconciler.

External context:

- Vercel preview deployments give each PR or branch a live URL, can use
  preview-scoped environment variables, and expose branch-specific and
  commit-specific generated URLs. Sources:
  [deployment methods](https://vercel.com/docs/deployments/deployment-methods),
  [generated URLs](https://vercel.com/docs/concepts/deployments/generated-urls),
  [Preview Deployments academy](https://vercel.com/academy/svelte-on-vercel/preview-deployments).
- Neon describes one database branch per preview, where a PR creates a matching
  database branch and injects the database URL into the preview environment.
  Sources:
  [branch-per-preview](https://neon.com/flow/branch-per-preview),
  [GitHub Actions guide](https://neon.com/docs/guides/branching-github-actions).
- Render preview environments create disposable copies of production-defined
  services, datastores, and environment groups for PRs, support initialization
  hooks, support expiry, and explicitly do not copy existing database data by
  default. Source:
  [Preview Environments](https://render.com/docs/preview-environments).
- Railway supports persistent and temporary PR environments; its Focused PR
  Environments only deploy services affected by files changed in the PR. Source:
  [Environments](https://docs.railway.com/reference/environments).
- GitHub's Deployments API models deployments and deployment statuses around
  refs, shas, environments, status states, environment URLs, and log URLs. That
  is a useful outer integration surface but not enough by itself for Ployz's
  stateful operation evidence. Sources:
  [deployments](https://docs.github.com/en/rest/deployments/deployments),
  [deployment statuses](https://docs.github.com/en/rest/deployments/statuses).

## Topic Axes

- PR source composition: how a branch environment decides which services,
  volumes, routes, and secrets are fresh, branched, portal-attached, omitted, or
  provider-backed.
- State and data safety: how production-like data, masking, source opt-in,
  read-only access, retention, and cleanup become explicit.
- Promotion and merge semantics: how a PR environment becomes production, or a
  production deploy adopts branch lineage, without pretending every promotion is
  atomic before the model can prove it.
- Automation lifecycle: how GitHub, Inngest, agents, warm pools, TTLs, retries,
  and push updates drive core primitives without becoming cluster controllers.
- Evidence and agent interface: how humans, cloud jobs, and coding agents see
  the same preview, apply, failure, checkpoint, and cleanup facts.

## Ranked Ideas

### 1. Branch Plan Compiler

**Description:** Add a first-class branch plan request that compiles
`source_namespace`, `target_namespace`, and per-resource source policies into a
deploy plan. The branch plan is not a new execution engine; it is a compiler
front-end over deploy intent, phase work, clone work, service source modes,
secrets policy, route policy, and cleanup policy. The output is a typed preview
that can be approved, applied, retried, and later explained.

**Axis:** PR source composition

**Basis:** `direct:` the deploy roadmap says deploy should compile high-level
operations such as "branch this namespace" into typed, previewable,
phase-aware plans; `direct:` the current service source plan is adding fresh vs
branch preview vocabulary; `external:` Render, Railway, Vercel, and Neon all
center PR workflows around automated environment creation from source branch
events.

**Rationale:** This is the core unlock. Cloud can expose presets like "fresh
branch", "clone database", "frontend-only PR", or "migration rehearsal", but
core still receives one explicit operation request with typed source policy per
resource. It prevents cloud from inventing private branching semantics.

**Downsides:** It creates a public branch-plan shape that must stay small. If it
turns into a generic workflow graph, it fights the product thesis.

**Confidence:** 94%

**Complexity:** High

**Status:** Unexplored

### 2. Resource Source Policy Matrix

**Description:** Make every resource in a branch plan resolve to one explicit
source mode: `fresh`, `branch`, `portal`, `provider_branch`, `seed`,
`shared_read_only`, `omit`, or `unsupported`. Services, volumes, secrets, and
routes can have different allowed modes, but the preview always shows the
resolved mode, source identity, target identity, safety class, and commit
behavior.

**Axis:** PR source composition

**Basis:** `direct:` the service source preview plan says source mode should be
typed planned work, not a warning; `direct:` the deploy roadmap distinguishes
fresh, branch, portal, move, migrate, promote, and checkpoint; `external:`
Render supports env overrides and init hooks, Neon supports provider-native
database branching, and Vercel supports branch-scoped preview configuration.

**Rationale:** Fixed environment modes are too coarse. A real PR environment
might run `web` fresh, branch `api`, snapshot-clone `postgres`, seed `redis`,
portal to shared `auth`, omit cron jobs, and publish preview-only routes. The
matrix makes that normal without creating one-off branch types.

**Downsides:** This requires clear support-status semantics. Public enum values
for unsupported modes create false promises unless preview explicitly rejects or
marks them reserved.

**Confidence:** 91%

**Complexity:** Medium

**Status:** Unexplored

### 3. Data Safety Ladder For PR Environments

**Description:** Treat PR data policy as a typed ladder rather than a checkbox:
`empty`, `seeded`, `snapshot_clone`, `masked_snapshot_clone`,
`provider_branch`, `shared_read_only`, and `blocked`. Each mode carries
preconditions, source opt-in, masking hooks, retention/TTL, credential scope,
and evidence about whether production-derived data entered the branch.

**Axis:** State and data safety

**Basis:** `direct:` `docs/future/cloud.md` already calls out ZFS clone,
provider-native branches, seed, empty, fresh, shared, masking, quotas, and TTLs;
`direct:` the deploy roadmap says production data policy is a product and
security boundary; `external:` Render avoids copying existing database data by
default, while Neon leans into branch-per-preview database branches.

**Rationale:** The dangerous version of stateful PR environments is "clone prod
because it is easy." The useful version is a visible data contract: exactly what
was copied, transformed, shared, or blocked, and why the branch is safe to use.

**Downsides:** Masking correctness cannot be proven generically. Core can model
that masking ran and what hook/hash was used; cloud or the operator still owns
the policy quality.

**Confidence:** 93%

**Complexity:** High

**Status:** Unexplored

### 4. PR Capsule Record

**Description:** Create a durable branch capsule for each PR environment:
source namespace, target namespace, git repo/ref/sha/PR number, branch plan
fingerprint, source resource revisions/snapshots, preview URLs, lifecycle TTL,
promotion eligibility, cleanup obligations, and current operation evidence. The
capsule is not desired state that reconciles itself; it is a durable handle for
explicit future commands.

**Axis:** Automation lifecycle

**Basis:** `direct:` `docs/future/cloud.md` says PR environments track source
commit, update on pushes, and destroy on close; `direct:` `docs/routing-and-deploys.md`
separates deploy status, deploy commits, phase records, lineage, movement
evidence, routing projections, and live probes; `reasoned:` agents need one
stable object to inspect before deciding whether to update, promote, destroy, or
retry a branch.

**Rationale:** This gives cloud and agents an operational memory without
creating a background controller. A GitHub event can look up the capsule and
compile the next explicit command: update, rerun validation, promote, suspend,
or destroy.

**Downsides:** The capsule boundary must be precise. If it starts owning cluster
truth instead of referencing deploy/lineage/route facts, it becomes a second
control plane.

**Confidence:** 88%

**Complexity:** Medium

**Status:** Unexplored

### 5. Promotion Candidate Pipeline

**Description:** Model a mergeable PR environment as a promotion candidate with
readiness evidence, source lineage, route policy, checkpoint policy, and
rollback capability. Early versions should support "production adoption from
branch lineage" as a normal production deploy. Later versions can support the
true `promote`: an atomic namespace traffic switch with a named rollback point.

**Axis:** Promotion and merge semantics

**Basis:** `direct:` the deploy roadmap explicitly distinguishes conservative
production adoption from final atomic promotion; `direct:` `VISION.md` names
branch, promote, and rollback as north-star primitives; `external:` Vercel
separates preview and production deployment flows, while established rollout
systems show the value of explicit readiness gates before production traffic
changes.

**Rationale:** This avoids overclaiming. Users can get useful "promote this PR"
UX early, but the evidence tells the truth: either this is a normal production
redeploy from branch lineage, or it is a stronger atomic traffic switch once the
model supports that guarantee.

**Downsides:** Product language must be disciplined. Calling the precursor
"promote" too early will create false expectations around rollback and traffic
switch atomicity.

**Confidence:** 89%

**Complexity:** High

**Status:** Explored

### 6. Rehearsal Branches For Dangerous Changes

**Description:** Make "rehearse this PR" a specialized branch plan that clones
production-like state, runs migrations or destructive setup in an isolated
branch, starts the proposed services, runs validation, and records whether the
candidate is safe to merge or promote. This differs from a normal preview
environment because its primary output is upgrade evidence, not a shareable URL.

**Axis:** Promotion and merge semantics

**Basis:** `direct:` deploy phases already include checkpoint and rollback
policy for DB upgrades and volume work; `direct:` `docs/future/cloud.md` places
migrations and masking after data service startup and before app startup;
`reasoned:` stateful systems need proof that a migration can run against
realistic state before production deploy time.

**Rationale:** This is where owning ZFS becomes product strategy. Ployz can make
database and volume upgrade rehearsals cheap enough to be default for risky PRs,
not a bespoke DBA ritual.

**Downsides:** Rehearsal evidence can give false confidence if data masking or
sampling removes the pathological rows that matter. The preview must describe
data provenance and limits.

**Confidence:** 86%

**Complexity:** High

**Status:** Unexplored

### 7. Focused PR Environment Updates

**Description:** On each PR push, recompile the branch capsule against changed
paths and current source truth, then update only the affected services or
resource policies. Unchanged services keep their current branch instances and
volumes unless the source policy or dependency graph requires refresh. The
preview should explain both changed and intentionally retained resources.

**Axis:** Automation lifecycle

**Basis:** `direct:` `docs/future/cloud.md` says PR updates should rebuild
changed images, recompile canvas, diff against running state, and deploy delta
only; `external:` Railway describes Focused PR Environments that deploy only
services affected by files changed in the PR.

**Rationale:** PR environments must be cheap in active repos. Delta update is
what makes stateful branches feel fast enough to use per commit instead of only
for special PRs.

**Downsides:** Changed-path inference is advisory, not truth. Core should accept
an explicit compiled update plan; cloud can own monorepo path heuristics and
show when it guessed.

**Confidence:** 84%

**Complexity:** Medium

**Status:** Unexplored

### 8. PR Operation Timeline

**Description:** Expose one timeline across branch creation, deploy updates,
validation, promotion attempts, checkpoint commits, cleanup, and residual
failures. Each event links to the underlying core evidence: deploy id, phase id,
resource source mode, source snapshot, participant result, route publication, or
cleanup artifact.

**Axis:** Evidence and agent interface

**Basis:** `direct:` `docs/routing-and-deploys.md` separates preview, apply,
phase records, commits, routing projections, cleanup, and failure-after-
checkpoint semantics; `external:` GitHub Deployment objects provide outer
environment/ref/status integration but do not capture stateful clone, masking,
checkpoint, or cleanup evidence.

**Rationale:** This is the surface agents need. "The PR env is broken" should
not require reading logs across GitHub, cloud jobs, deploy commits, and daemon
RPC traces. The timeline should explain which primitive failed and what command
can safely run next.

**Downsides:** It is easy to accidentally create a second event schema in cloud.
The timeline should be a presentation over core operation evidence plus cloud
workflow metadata.

**Confidence:** 87%

**Complexity:** Medium

**Status:** Unexplored

### 9. Warm Branch Pools

**Description:** Pre-create branch capsules through the expensive phases:
source snapshot, cloned volumes, base services, secrets fetch, and baseline
readiness. When a PR opens, claim a warm branch, bind it to the git ref, apply
changed services, run branch-specific hooks, and publish routes.

**Axis:** Automation lifecycle

**Basis:** `direct:` `docs/future/cloud.md` proposes warm pools to reduce
spin-up by pre-provisioning phases; `reasoned:` ZFS makes state copies cheap,
but build/pull/start/readiness and masking hooks still create perceived latency.

**Rationale:** This turns stateful PR environments from "available after the
coffee break" into something closer to frontend preview deployment speed. It
also gives agents a fast sandbox to test speculative changes.

**Downsides:** Warm pools create resource leakage risk. They need strict TTL,
quota, source freshness, and destruction evidence.

**Confidence:** 78%

**Complexity:** High

**Status:** Unexplored

### 10. Preview Access Boundary As A Primitive

**Description:** Treat preview access as part of the branch plan: public,
team-only, invite-only, source-namespace-authorized, or no public route. Route
publication, secret exposure, portal access, and data policy should all be
validated together before a PR environment becomes reachable.

**Axis:** State and data safety

**Basis:** `direct:` portal preview is intentionally rejected until
authorization, redaction, and source opt-in are designed; `external:` Vercel
supports protected preview deployments; `reasoned:` PR environments containing
production-derived or shared state need access control coupled to data source
policy, not only to web routing.

**Rationale:** A branch with masked seed data and a branch with production
read-only portal access should not get the same URL policy. Access should be
derived from source modes and data policy, then shown in preview.

**Downsides:** This crosses core/cloud boundaries. Core can model route/access
requirements and reject unsafe combinations; cloud likely owns identity, team,
and invitation mechanics.

**Confidence:** 82%

**Complexity:** Medium

**Status:** Unexplored

## Strong Workflow Futures

### Frontend-Only PR

- `web`: fresh from PR image
- `api`: portal to staging or production-compatible backend
- `postgres`: omitted
- routes: preview URL only
- promotion: adopt `web` into production deploy

This should be cheap and fast. It mostly tests UI and route behavior.

### Full-Stack PR

- `web`, `api`, `worker`: fresh or branch-derived from source namespace
- `postgres`: masked snapshot clone
- `redis`, queues: fresh
- cron jobs: disabled
- routes: protected preview URL
- validation: smoke tests and migration checks

This is the normal "review the whole change realistically" path.

### Migration Rehearsal PR

- data volumes: clone from production or staging checkpoint
- migrations: run in checkpoint phase
- app services: start only after migration phase commits
- output: readiness and migration evidence, not necessarily public preview
- promotion: blocked unless rehearsal evidence matches source revision

This is the safety path for schema and data changes.

### Production Adoption From Branch

- branch candidate already exists
- production deploy references branch service/volume lineage
- preview shows exactly what production will adopt
- apply uses normal deploy lifecycle
- rollback is whatever the deploy checkpoint policy can honestly support

This is the conservative precursor before atomic promotion.

### Atomic Namespace Promotion

- branch candidate already has readiness, route, lineage, and rollback point
- promote validates source and target freshness
- one promotion commit switches production traffic identity
- old production remains retained as rollback target

This is the north-star path, not the first implementation slice.

## Rejection Summary

| # | Idea | Reason Rejected |
|---|------|-----------------|
| 1 | General workflow DSL in core | Scope overrun. Recurring PR workflows should become named primitives, not arbitrary workflow graphs. |
| 2 | Background PR reconciler in core | Violates `VISION.md`; GitHub events or cloud jobs may call commands, but the cluster should not silently advance desired state. |
| 3 | Treat portal as branch-lite | Rejected on safety grounds. Portal is live attachment, not lineage, and needs source opt-in plus authorization. |
| 4 | Clone production data by default | Too dangerous relative to value. Data policy must be explicit, and managed cloud should bias toward masked/seeded/safe modes. |
| 5 | One fixed PR environment mode | Duplicates weaker version of resource source matrix. Real apps need per-resource source modes. |
| 6 | Promote by mutating production namespace in place | Duplicates weaker version of production adoption. Useful as a precursor only when evidence calls it redeploy/adoption, not atomic promote. |
| 7 | Store raw deploy manifests as PR evidence | Conflicts with existing deploy docs because service specs may contain sensitive values. Store typed evidence and hashes instead. |
| 8 | Make GitHub Deployment status the primary operation record | Not enough for Ployz. GitHub is a useful integration surface, but stateful clone/mask/checkpoint evidence belongs in core/cloud records. |
| 9 | Auto-refresh PR branches from production nightly | Too close to hidden reconciliation. Make refresh an explicit command or scheduled cloud job that records a new operation. |
| 10 | Let cloud own branch semantics first and backfill core later | Architecture violation. Cloud should compile to core primitives so CLI, SDK, and agents share the same semantics. |
| 11 | Environment templates as the core abstraction | Too cloud-shaped. Templates can generate plans, but core should expose operation primitives and typed resource policies. |
| 12 | Treat warm pools as always-on required infrastructure | Too expensive and optional. Warm pools are an optimization over branch plans, not a dependency of branching. |
| 13 | Preview all portal source topology before authorization | Security violation. Unauthorized portal preview can become cross-namespace enumeration. |
| 14 | Full atomic promotion as the next slice | Too expensive relative to current model maturity. Production adoption from branch lineage is the pragmatic stepping stone. |

## Strongest Next Brainstorm Targets

1. **Branch Plan Compiler**: define the request/preview model and what compiles
   into deploy manifest intent vs deploy-adjacent operation metadata.
2. **Resource Source Policy Matrix**: define the allowed service, volume,
   secret, and route source modes plus support-status semantics.
3. **Data Safety Ladder**: define production-derived data policies before cloud
   advertises cloned PR databases.
4. **Promotion Candidate Pipeline**: define the honest precursor to `promote`
   and the evidence required before the atomic version can exist.
