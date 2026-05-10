---
date: 2026-05-10
topic: dev-authority
focus: local and cloud development environments driven by Ployz authorities, agent ergonomics, framework adapters, service control, and file synchronization
mode: repo-grounded
status: open
---

# Ideation: Dev Authority

## Grounding Context

Ployz is an explicit-command orchestration core for small clusters. `VISION.md`
already names `ployzctl dev` as a north-star primitive that should work on a
single developer Mac, a small office mesh, and a fleet of machines with the same
model. `docs/authority-roadmap.md` also reserves "Dev authority" as a later
role that owns local truth, with remote writes failing when the remote authority
is unreachable.

That means local development should not become a separate product bolted beside
the orchestrator. The strongest version is: a developer computer can be a real
Ployz authority, with a local control-plane truth store, a local or remote data
plane, and explicit operations for starting, stopping, moving, branching,
proxying, syncing, inspecting, and cleaning up a development environment.

The existing architecture gives this idea unusual leverage:

- Ployz already models authorities as ownership, not geography.
- Stored intent, projections, live facts, and health metrics are already
  separated.
- Deploy manifests already know services, ports, network mode, readiness,
  routes, mounts, volumes, placement, branch/move/portal service intents, and
  clone/move volume intents.
- The runtime backend already has Docker and host-oriented seams, plus data-plane
  sidecar supervision and adoption semantics.
- The future macOS host access note already points toward a local userspace
  WireGuard tunnel and DNS resolver.
- The project direction requires agent-compatible operation surfaces: typed
  results, visible preconditions, explicit verification, and no hidden
  reconcilers.

External grounding shows adjacent products solving pieces of this, but not the
whole primitive:

- Dev Containers and Codespaces standardize repository-defined toolchains,
  ports, and IDE setup.
- Coder and DevPod move full workspaces onto managed or user-controlled compute.
- Mutagen solves low-latency bidirectional file synchronization between a laptop
  and remote containers or hosts.
- Okteto, Garden, and Skaffold provide remote Kubernetes dev loops with file
  sync, hot reload, logs, and port forwarding.
- Telepresence and mirrord make local processes run in, or receive traffic from,
  a remote cluster context.
- Dagger makes workflows local-first, repeatable, observable, and runnable on a
  laptop, agent sandbox, CI server, or cloud engine.

Ployz's opportunity is not to copy any one of these. It is to make development
environments movable operational objects with first-class state, networking, and
agent-control primitives.

## Topic Axes

- Local authority model
- Service graph and framework adapters
- Local, remote, and hybrid execution
- File synchronization and source truth
- Data services and branchable state
- Agent-facing operations
- Cross-authority development workflows
- macOS/Linux ergonomics

## Headline Bets

The five ideas that feel most product-defining are:

1. Treat the developer machine as a real Ployz authority, not a special local
   mode.
2. Make `ployzctl dev` quiet by default and expose service-level operations for
   humans and agents.
3. Make source sync a first-class operation so local files can drive remote
   compute.
4. Treat local process, proxy, devcontainer, local container, and remote machine
   execution as explicit placement choices.
5. Make branchable dev data a core primitive, not just "start Postgres in
   Docker."

## Ranked Ideas

### 1. Dev Authority as the Unifying Primitive

**Description:** `ployzctl dev` creates or attaches to a local authority for the
repository. That authority owns dev-environment truth: project identity, service
graph, selected execution mode, dependency instances, routes, sync sessions,
remote links, and operation history.

**Basis:** `direct:` `VISION.md` says the same primitives should work on a
developer Mac and in the cloud; `direct:` `docs/authority-roadmap.md` names dev
authority as stored intent; `reasoned:` local development needs ownership and
auditable operations if it will be agent-friendly.

**Why it is strong:** This makes local dev a real product surface instead of a
CLI convenience. The laptop can run everything locally, proxy local processes
into a Ployz network, or move work to a remote machine without changing the
operator model.

**Concrete shape:**

- `ployzctl dev init` discovers a project and writes a dev authority record.
- `ployzctl dev up` starts the selected topology.
- `ployzctl dev status --json` reports stored intent plus live observations.
- `ployzctl dev down` stops selected dev resources without deleting state.
- `ployzctl dev destroy` removes local truth and disposable data.
- `ployzctl dev link <authority>` grants this dev authority explicit permission
  to talk to a remote authority.

**Guardrail:** Do not let `dev up` become a background reconciler. It should
perform a bounded operation, report what changed, and return commands for later
inspection.

**Confidence:** 92%

**Complexity:** Medium

**Status:** Strong candidate

### 2. Development Topology Capsule

**Description:** A repo has a Ployz dev topology capsule: the service graph,
dependency graph, runtime adapters, ports, env sources, volumes, sync rules,
commands, readiness checks, and supported execution modes. It can be generated
from repo evidence, but once accepted it becomes explicit local authority intent.

**Basis:** `direct:` deploy manifests already model services, ports, readiness,
routes, volumes, and intents; `external:` devcontainers and Codespaces use
`devcontainer.json` for tailored development environments; `reasoned:` agents
need a stable manifest more than they need unbounded log streams.

**Why it is strong:** This turns "how do I run this project?" into structured
data. It can support Laravel, Next.js, Rails, Go, Rust, Python, Docker Compose,
and devcontainers without pretending every project is the same.

**Concrete shape:**

```toml
[project]
name = "shop"
schema = "laravel"

[[services]]
name = "web"
adapter = "herd-proxy"
port = 8000
depends_on = ["postgres", "redis"]

[[services]]
name = "vite"
adapter = "process"
command = "npm run dev"
port = 5173

[[dependencies]]
name = "postgres"
kind = "postgres"
state = "branchable"

[[modes]]
name = "remote-compute"
placement = "cloud"
source = "sync"
```

**Guardrail:** Inference can propose a capsule, but it must not be durable truth
until the user or agent accepts it. Missing confidence should be visible.

**Confidence:** 88%

**Complexity:** Medium

**Status:** Strong candidate

### 3. Execution Modes as Placement, Not Product Forks

**Description:** The same service can run in multiple execution modes:
local-process, local-container, devcontainer, remote-container, remote-machine,
or proxy-only. Moving between them is a placement operation, not a separate
configuration system.

**Basis:** `direct:` Ployz already treats regions as placement metadata and
authorities as ownership; `external:` DevPod can target a laptop, cloud
provider, or Kubernetes cluster; `reasoned:` developers will not accept one
blessed runtime path.

**Why it is strong:** This is the concept that makes Herd, `npm run dev`,
Docker Compose, devcontainers, and remote machines feel like one product. Ployz
can support pragmatic local workflows while keeping the control model clean.

**Concrete shape:**

- `ployzctl dev move web --to local-process`
- `ployzctl dev move web --to remote --machine dev-8xl`
- `ployzctl dev move postgres --to local-container`
- `ployzctl dev proxy web --from localhost:8000 --as service:web`
- `ployzctl dev replace web --with remote-container`

**Guardrail:** "Placement" must remain explicit. Ployz should not silently move
work off a laptop because CPU is high or a remote machine exists.

**Confidence:** 90%

**Complexity:** High

**Status:** Strong candidate

### 4. Source Sync Session as a First-Class Operation

**Description:** Add a source sync primitive for remote dev: an explicit session
that moves selected local filesystem changes into a remote runtime, reports
lag/conflicts/errors, can be paused/resumed, and can be verified by agents.

**Basis:** `external:` Mutagen focuses on bidirectional, low-latency file
synchronization between laptops and remote containers; `external:` Okteto and
Garden use file sync/hot reload for remote development loops; `reasoned:` this
is the missing piece for "local files, cloud runtime" to feel instant.

**Why it is strong:** It lets developers and agents keep editing the local
checkout while running compute, dependencies, and network access remotely. It is
also a better fit for agents than SSHing into a disposable workspace, because
the user's local filesystem remains the editing authority.

**Concrete shape:**

- `ployzctl dev sync start --target remote:web`
- `ployzctl dev sync status --json`
- `ployzctl dev sync pause`
- `ployzctl dev sync resume`
- `ployzctl dev sync conflicts`
- `ployzctl dev sync verify --path app/Http/Controllers/HomeController.php`

**Design stance:** Consider embedding or integrating a proven sync engine early
rather than building file watching, deltas, ignores, case-sensitivity handling,
symlink policy, and conflict detection from scratch.

**Guardrail:** Sync state is live operational state, not the source of truth for
the repo. Git and the local filesystem remain the user's source authority unless
they explicitly choose a remote workspace mode.

**Confidence:** 91%

**Complexity:** High

**Status:** Strong candidate

### 5. Agent Control Surface: Quiet by Default, Queryable on Demand

**Description:** `ployzctl dev up` should not become a noisy log tail. It should
return a compact service table, next commands, structured operation IDs, and
machine-readable handles for logs, restart, shell, env, health, traces, sync,
and test runs.

**Basis:** `direct:` Ployz's vision says operation surfaces are first-class and
agent-usable; `external:` Dagger emphasizes observable, repeatable operations
with detailed traces; `reasoned:` agents need handles and structured summaries,
not thousands of mixed logs.

**Why it is strong:** This could be the difference between "an agent can maybe
run the app" and "an agent can operate the app." Every service becomes a small
control surface.

**Concrete shape:**

- `ployzctl dev logs web --since 2m --level warn --json`
- `ployzctl dev restart vite`
- `ployzctl dev exec web -- php artisan migrate`
- `ployzctl dev health --wait --json`
- `ployzctl dev open web`
- `ployzctl dev test unit --service web`
- `ployzctl dev evidence <operation-id>`

**Guardrail:** Logs are evidence, not an audience. Failures should surface in
status and operation evidence with an explicit next action.

**Confidence:** 94%

**Complexity:** Medium

**Status:** Highest-leverage agent idea

### 6. Branchable Dev Data Services

**Description:** Ployz dev can boot dependencies like Postgres, Redis, MySQL,
NATS, MinIO, and local object stores as named services, then snapshot, fork,
reset, seed, and move them through the same volume primitives used by
production-like deployments.

**Basis:** `direct:` Ployz's substrate bet is ZFS/Btrfs snapshots and
branchable state; `direct:` deploy manifests already have volume clone/move
intents; `external:` mirrord for Teams advertises database branching as part of
team development context; `reasoned:` app dev is bottlenecked by state more
than process startup.

**Why it is strong:** Most dev tools can start a Postgres container. Ployz can
make "give this agent a disposable branch of production-like data, run a
migration, then throw it away" a primitive.

**Concrete shape:**

- `ployzctl dev data fork prod-postgres --as issue-123`
- `ployzctl dev data reset postgres --to seed:baseline`
- `ployzctl dev data snapshot postgres --name before-agent`
- `ployzctl dev data diff postgres before-agent after-agent`
- `ployzctl dev data promote seed:clean`

**Guardrail:** This needs sharp safety boundaries. Production data import,
masking, secrets, and permission to fork remote data must be explicit authority
operations with typed preconditions.

**Confidence:** 86%

**Complexity:** High

**Status:** Strong differentiator

### 7. Ployz Portal: Cloud-to-Local and Local-to-Cloud Network Plumbing

**Description:** Add a "portal" primitive for development traffic. A local
process can appear as a service inside the Ployz network, a remote service can
appear on localhost, and HTTP/TCP routes can be exposed with stable names and
agent-readable handles.

**Basis:** `direct:` deploy manifests already include a `portal` service intent
and network modes; `direct:` macOS host access is already a future design note;
`external:` Telepresence routes remote service traffic to local environments,
while mirrord runs local processes in a remote pod context.

**Why it is strong:** Ployz already owns the overlay and gateway. That makes it
credible to provide a smoother "this local process is now in the dev mesh"
experience across macOS and Linux without forcing every service into a
container.

**Concrete shape:**

- `ployzctl dev portal create web --local localhost:8000 --service web`
- `ployzctl dev forward postgres --to localhost:5432`
- `ployzctl dev route web --host shop.dev.ployz.test`
- `ployzctl dev intercept checkout --to local:web --filter header:x-user=nick`
- `ployzctl dev share web --ttl 2h`

**Guardrail:** Intercepts and portals must be explicit and revocable. No
ambient cluster-wide VPN behavior should surprise the user or agent.

**Confidence:** 89%

**Complexity:** High

**Status:** Strong candidate

### 8. Cross-Authority Dev Branches

**Description:** A dev authority can ask a remote authority for a branch of an
environment: clone needed data, provision remote dependencies, route selected
services, and expose the result back to the local CLI. Mutations fail fast when
the remote authority is unreachable.

**Basis:** `direct:` authority rules already say remote mutations never queue;
`direct:` branch, promote, rollback, and fork-volume are north-star primitives;
`reasoned:` this is how local development connects to real cloud resources
without losing ownership clarity.

**Why it is strong:** This creates a path from laptop dev to PR environments to
cloud preview environments with the same primitives. It also creates a clean
authorization story: the local authority has permission to ask for specific
remote operations, not broad cloud access.

**Concrete shape:**

- `ployzctl dev branch remote/prod --as nick-login-fix`
- `ployzctl dev attach nick-login-fix`
- `ployzctl dev move web --to local-process`
- `ployzctl dev move worker --to remote`
- `ployzctl dev promote nick-login-fix --target staging`

**Guardrail:** A local dev authority must not cache remote write intent for
later replay. If the remote authority cannot confirm the branch, the command
fails before local mutation depends on it.

**Confidence:** 87%

**Complexity:** High

**Status:** Strategic candidate

### 9. Framework Packs as Adapters, Not a Plugin Marketplace

**Description:** Ship opinionated framework adapters for Laravel, Next.js,
Rails, Django, Phoenix, Go services, Rust services, and generic processes. Each
adapter knows how to detect the project, propose services, expose useful
commands, discover ports, and map common dependency names.

**Basis:** `external:` DevPod can analyze a project to set up a best-estimate
environment; `external:` Codespaces has predefined dev container configurations;
`reasoned:` Ployz needs a great first-run experience without turning into a
generic plugin assembly kit.

**Why it is strong:** The product can feel magic for common stacks while still
recording explicit topology after discovery. Laravel Herd proxy support is a
good example: Ployz does not need to replace Herd, it can adopt it as a service
implementation.

**Concrete shape:**

- Laravel: Herd/Valet proxy mode, Sail/container mode, artisan commands,
  migrations, queues, Redis/Postgres/MySQL dependencies, Vite service.
- Next.js: `npm run dev`, route discovery, Turbopack/Vite handling, env
  surfaces, optional remote build cache.
- Rails: Puma, Sidekiq, Postgres/Redis, migrations, asset watcher.
- Generic process: command, port, env, health, restart, logs.
- Devcontainer: use `.devcontainer/devcontainer.json` as a runtime adapter
  rather than the whole product model.

**Guardrail:** Framework packs should not become hidden policy engines. Their
job is to propose topology and expose commands, not continually rewrite local
truth.

**Confidence:** 84%

**Complexity:** Medium

**Status:** Important adoption layer

### 10. Remote Compute Burst for Agents

**Description:** Let an agent keep working in the local repo but burst heavy
workloads into a remote Ployz machine: app servers, databases, browsers, test
clusters, build jobs, or GPU-heavy tasks. Results stream back through operation
evidence and local ports.

**Basis:** `external:` Coder moves environments to managed infrastructure for
speed, consistency, and security; `external:` Dagger runs workflows on laptops,
AI sandboxes, CI servers, or dedicated cloud infrastructure; `reasoned:` agents
often need stronger Linux compute than the user's laptop has.

**Why it is strong:** This is likely a killer agent workflow. The agent edits
locally, syncs remotely, runs realistic tests remotely, reads structured
failure evidence, and restarts individual services without occupying the
developer's laptop.

**Concrete shape:**

- `ployzctl dev compute add --size 8x`
- `ployzctl dev run e2e --on remote --browser chromium`
- `ployzctl dev move all --to remote --proxy-back`
- `ployzctl dev compute sleep`
- `ployzctl dev compute remove --preserve-snapshots`

**Guardrail:** Remote compute should be an explicit placement target with cost,
permission, and cleanup surfaced. Avoid hidden "smart offload."

**Confidence:** 88%

**Complexity:** High

**Status:** Strategic candidate

### 11. Dev Environment Evidence Ledger

**Description:** Every dev operation writes evidence: discovered topology,
selected adapter, command started, pid/container/remote placement, sync target,
readiness checks, exposed routes, dependency state, and failure audience.

**Basis:** `direct:` prior deploy-process ideation identified an operation
ledger as a strong idea; `direct:` documented solutions require separating truth
from observation and preflighting final participants before mutation.

**Why it is strong:** It makes dev environments inspectable after the fact. That
is useful for humans, but essential for agents that need to recover from partial
failure without reading terminal scrollback.

**Concrete shape:**

- `ployzctl dev ops`
- `ployzctl dev evidence op_01HX...`
- `ployzctl dev why web`
- `ployzctl dev diagnose --since last-failure`

**Guardrail:** Evidence should describe bounded operations. Do not create a
forever event firehose as a substitute for clear operation results.

**Confidence:** 85%

**Complexity:** Medium

**Status:** Strong support primitive

### 12. Team and Agent Session Isolation

**Description:** A dev authority can mint isolated sessions for humans and
agents. A session gets its own routes, data branches, sync session, service
overrides, secrets scope, and cleanup policy.

**Basis:** `external:` mirrord for Teams emphasizes concurrent usage, traffic
filtering, queue splitting, session management, and ephemeral DB branches;
`reasoned:` agent concurrency will make shared dev environments messy unless
session isolation is a primitive.

**Why it is strong:** This avoids "the agent broke my local dev server" and
"two agents are writing to the same database" failure modes. It also lets a
developer hand a bounded workspace to an agent without giving it broad local or
cloud access.

**Concrete shape:**

- `ployzctl dev session create agent-login-fix --branch-data`
- `ployzctl dev session grant agent-login-fix service:web,data:postgres`
- `ployzctl dev session logs agent-login-fix`
- `ployzctl dev session destroy agent-login-fix`

**Guardrail:** Sessions are isolation and permission surfaces, not hidden
background operators.

**Confidence:** 82%

**Complexity:** High

**Status:** Later-stage differentiator

## Rejected or Deferred Ideas

### Full IDE Replacement

Ployz should not build an editor, terminal multiplexer, or browser IDE. Existing
editors are good, and the repo vision says the core should expose primitives
that downstream tools consume. Ployz should make local and remote execution
operable from any editor.

### Kubernetes Compatibility Layer

Copying Skaffold, Telepresence, Okteto, or Garden around Kubernetes would fight
the project's premise. The useful ideas are file sync, intercepts, port
forwarding, and remote context. The product model should stay Ployz authorities,
services, routes, volumes, and operations.

### Docker Compose Clone

Compose is useful input and a possible adapter, but becoming another Compose
runner would make Ployz less special. Ployz should adopt Compose-shaped service
graphs when present, then expose richer state, routing, movement, and agent
operations.

### Pure Magic Project Inference

Automatic detection is valuable for first-run experience, but durable local
truth cannot be "whatever the detector thinks today." Inference should produce a
reviewable topology capsule with confidence and gaps.

### Continuous Desired-State Dev Loop

A watch loop that continually rewrites services toward a manifest would import
the reconciler model Ployz is trying to avoid. Watchers are fine for file sync,
logs, and live observation; topology and placement changes should be explicit
operations.

### Always-Remote Workspaces

Remote workspaces are valuable, but forcing all dev into remote compute misses
the strong local adoption path. The better model is movable execution: local
when it is fast and comfortable, remote when it needs Linux, power, cloud
network access, or agent isolation.

## Most Promising Product Shape

The product should feel like this:

```text
$ ployzctl dev up

authority  shop@macbook
mode       hybrid
routes     web -> https://shop.dev.ployz.test
services   web        local Herd proxy      ready
           vite       process npm run dev   ready
           worker     remote container      ready
           postgres   remote branch         ready
           redis      local container       ready
sync       active to remote worker, lag 120ms

next:
  ployzctl dev status --json
  ployzctl dev logs web --since 2m
  ployzctl dev restart worker
  ployzctl dev sync status
  ployzctl dev evidence op_01HX...
```

The command starts the environment and then gets out of the way. It does not
dump all logs. It gives a human or agent handles to ask precise questions and
take precise actions.

## Suggested First Slice

The first serious slice should avoid remote cloud movement until the local model
is crisp:

1. Local dev authority identity and state directory.
2. Topology capsule format for process, proxy, container, and dependency
   services.
3. Quiet `dev up`, `dev status`, `dev logs`, `dev restart`, `dev exec`, and
   `dev down`.
4. One framework adapter, preferably Laravel because Herd/proxy plus
   Postgres/Redis/Vite exercises the hybrid model.
5. One generic process adapter for Next.js or `npm run dev`.
6. Local dependency services for Postgres and Redis.
7. Structured JSON output and operation evidence from day one.

The second slice should add file sync plus a remote machine target. That is
where the concept becomes clearly different from local process managers.

## Open Questions for Brainstorming

- Is the durable topology file a Ployz-specific file, an extension to deploy
  manifests, or a separate dev capsule that can compile into deploy manifests?
- Should source sync be an embedded engine, an integration with Mutagen, or a
  narrow interface that can support multiple engines?
- What is the minimum macOS networking story: userspace WireGuard plus DNS,
  localhost forwards only, or both?
- How much of Laravel Herd/Valet should Ployz detect and adopt versus asking
  the user to declare?
- What authority permission model lets a laptop branch remote data without
  creating dangerous ambient cloud credentials?
- Should an agent session own a git worktree, a data branch, both, or neither
  by default?
- Where is the line between `ployzctl dev` and `ployzctl branch` for PR
  environments?

## Sources

- `VISION.md`
- `docs/authority-roadmap.md`
- `docs/architecture.md`
- `docs/routing-and-deploys.md`
- `packages/deploy/index.d.ts`
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`
- GitHub Codespaces dev container documentation:
  https://docs.github.com/en/codespaces/setting-up-your-project-for-codespaces/adding-a-dev-container-configuration/introduction-to-dev-containers
- Coder documentation:
  https://coder.com/docs/about
- DevPod:
  https://website.devpod.sh/
- Mutagen synchronization:
  https://mutagen.io/documentation/synchronization
- Okteto file sync mode:
  https://www.okteto.com/docs/1.34/development/containers/file-sync/
- Telepresence intercepts:
  https://telepresence.io/docs/2.20/howtos/intercepts/
- mirrord introduction:
  https://metalbear.com/mirrord/docs/overview/introduction
- Dagger overview:
  https://docs.dagger.io/
