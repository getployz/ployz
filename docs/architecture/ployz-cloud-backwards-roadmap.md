# Ployz Core Roadmap — Built Backwards From Cloud

> Status: synthesis, 2026-06-01. Supersedes the **sequencing** in
> `docs/architecture/ployz-1-0-roadmap.md` while preserving all of its hard
> architecture invariants and VISION.md's primitive surface. This revision
> folds in three independent stress-test reviews (dependency-ordering,
> cloud-unblock, minimality); see the changelog for accepted/rejected findings.

## Framing

The target is **not** the pre-existing 1.0 cut. The target is the **minimum
deployment orchestrator** that fully supports what `ployz-cloud` actually does
today and is explicitly building toward: provision and dispose of machines,
deploy environments atomically, observe runtime truth, route HTTPS, manage
persistent state, and **clone environments**.

Minimality is a first-class value, and it is in tension with the user's
explicit headline targets (disposable machines, env cloning). The resolution
this revision adopts: **deliver every named target, but on the cheapest path
that works**, and push every heavy substrate primitive (ZFS copy-on-write,
cross-machine volume move, a durable namespaces lineage table) behind an
explicit "only when a measured need exists" gate rather than onto the critical
path. Where the 1.0 roadmap defers something cloud truly needs, we pull it
forward; where 1.0 plans something cloud does not need, we drop it or keep it
deferred.

### The seam (one sentence)

Cloud reaches the core **only** over SSH: `ssh user@host ployzctl <cmd>` and
`ployzctl rpc-stdio` (one JSON line in, one `{ok,code,message,payload}` line
out), plus a long-lived `ployzctl runtime stream --json`. The shared data
contract is the `@ployz/deploy` npm package. Today **every endpoint cloud
targets is missing or reshaped on the core side** — `crates/ployzd/src/main.rs`
is an 8-line stub that returns `ExitCode::FAILURE` and there is no `ployzctl`
binary. Cloud is built against a deleted legacy daemon.

### Starting line (honest, verified against the tree)

- **Real and tested:** the Corrosion + iroh substrate (`polis::store`,
  `corrosion_agent`, `peers`, `membership` with `IslandId`/`MachineRow`), the
  disposable-daemon adopt-not-recreate contract (for Corrosion + iroh
  identity), and a body of correct product *logic* over traits (deploy
  plan/observe/apply, machine join, ACME attempt idempotency, domain
  readiness, volume transfer fencing, an authority/claims boundary).
- **Scaffold / missing:** no CLI, no `clap`, no runnable daemon, no real
  container runtime, no real ACME CA, no gateway/DNS, no ZFS, no RoutingState
  stream, **no store-health or sync-connectedness signal in `polis`**, **no
  WireGuard controller** (membership stores only a `wireguard_public_key`
  text field), **no durable operation record** (`OperationId` is a bare
  newtype; there is no `OperationStatus`/`OperationStage`/`list_operations`).
- The core's current `DeployManifest` (deploy/mod.rs) is a single-route HTTPS
  activation request, and `DeployPlan` is a **3-axis** plan over
  `(domain, runtime, serving)`, each `Current(T) | Apply` — **not** a linear
  5-phase pipeline. This matters: the deploy phase contract must be reconciled
  before any RPC exposes it (see M3 and Open Questions).
- `RuntimePort` exposes only `activate_participant` / `verify_participant` —
  there is no stop/cleanup/cancel primitive yet.
- `MEMORY.md` is stale (it describes the deleted legacy MVP). The
  `RuntimePort` / `ServingActivationPort` / `CertificateAuthorityPort` traits
  are real; their only implementors are fakes.

## Non-Negotiable Invariants (carried through every milestone)

1. **polis(substrate) vs ployz(product) boundary.** All cloud sequencing lives
   in `crates/ployz/src/adapters/polis/` over narrow primitives. No
   product-shaped polis APIs. No ordinary ployz module imports
   Corrosion/iroh/irpc. **`polis` gains no `network`/`org`/`slug` concept**:
   the "network name == org slug" identity is a *ployz product* concept mapped
   to a `polis::IslandId` in `adapters/polis/`.
2. **Disposable daemon / adopt-not-recreate.** ployzd restart must not disturb
   WireGuard, iroh, gateway, DNS, or workloads; startup adopts what is running.
3. **State derived from Corrosion rows.** Service/route/namespace views are
   reconstructed from container + certificate (+ volume) rows. The only
   deliberate exception in the critical path is a minimal deploy-commit /
   `coreDeployId` correlation record (M3+), an owned table with one clear
   writer (the coordinator), not a desired-state model.
4. **No hidden controller.** Every state change is an explicit preview/apply
   command with a clear return. No background reconciler rewrites cluster
   truth. Liveness is probed at decision time, never inferred from freshness
   timestamps into durable truth.
5. **Trust boundaries are separate.** `rpc-stdio` is the EXTERNAL boundary
   behind an authorization check; internal peer RPC keeps its own typed
   protocol and never deserializes public requests.
6. **Deadlines on every external control-plane I/O** (Docker, iroh, SSH, HTTP,
   ZFS, process waits). Stalls are treated as failure within a bounded
   deadline.
7. **Atomicity with audience.** Failed-before-commit vs failed-after-checkpoint
   states are visible and recoverable. Half-applied-state-served-as-success is
   the worst outcome and is explicitly disallowed.
8. **Confidentiality is honest.** Plaintext env injected via the manifest is
   replicated by Corrosion to every mesh node's SQLite. The core keeps it
   confidential **in transit (SSH)** but it is **plaintext at rest on every
   node**. The core makes no "encrypted at rest" claim; cloud's threat model
   must reflect this (see Open Questions / secrets).

## Milestone Sequence

Milestones are sized so each ships independent value. Heavy net-new subsystems
(WireGuard controller, container runtime, ACME CA, gateway, ZFS) each own a
milestone or a clearly-named sub-milestone rather than hiding inside a bullet.

### M0 — Honest starting line: substrate spine + health/sync signal
*(substrate already real; health signal is net-new)*

**Serves:** disposability via live probe; the readiness aggregation M2a needs.

**Deliverables:**
- Lock in `polis::store` / `corrosion_agent` / `peers` / `membership` and
  `ployzd` substrate boot (already real and tested).
- Fix the ployzd lib-test parallel flake (concurrent real corrosion agents);
  pin/vendor the corrosion binary for clean-checkout reproducibility.
- **New, explicit:** a `polis` **store-health probe + sync-connectedness
  signal** primitive. This does **not** exist today (`store.rs` exposes only
  `execute_transaction`/`query`/`subscribe`/`updates`); the corrected starting
  line is that `polis` reports *no* health. M2a's `MeshReady` cannot aggregate
  health it cannot read, so the primitive lands here.

**Done when:** substrate-spine + machine_add e2e pass reproducibly under
parallel run; restart preserves endpoint id; the corrosion binary resolves in
a clean checkout; a node can answer "is my store healthy / am I sync-connected"
as a structured signal, probed at call time.

**Depends on:** —

### M1 — `ployzctl` CLI + `rpc-stdio` external contract + `Status` + schema/installer ownership

**Serves:** SSH-exec binary, rpc-stdio envelope, Status, the wire-contract
generation pipeline, and the curl installer — all first-touch cloud
dependencies.

**Deliverables:**
- Root `ployzctl` crate/binary (ship `ployzctl`; alias `ployz`); runnable
  system-mode `ployzd` replacing the 8-line stub and composing the M0
  substrate.
- `rpc-stdio` entrypoint: read one JSON request line, write one
  `{ok,code,message,payload(kind-tagged)}` response, as the EXTERNAL trust
  boundary behind an authorization check. Typed external request enum,
  distinct from internal peer RPC.
- Code taxonomy reproducing the exact strings cloud branches on, starting with
  `UNKNOWN_COMMAND`; additive-only thereafter.
- `Status` payload `{machine_id,daemon_version,active_network_name,phase,
  capabilities[]}` from `StartupReport` + iroh identity. `status`/`doctor`
  human+JSON commands; exit-code contract (0..5).
- **`capabilities[]` registry (pinned, additive).** Kept, but defined as a
  single versioned token vocabulary that every later milestone appends to.
  Ships present-but-minimal so it is not load-bearing before there is a
  feature `UNKNOWN_COMMAND` tolerance cannot express. (Rejecting the minimality
  call to drop it: cloud already reads `RpcDaemonStatusPayload.capabilities[]`
  directly, so the field is a real wire contract, not speculative — but it
  must be governed.)
- **Wire-contract generation pipeline (pulled forward from M3).** Stand up the
  JSON-Schema/`.d.ts` export step now and make ployz-rust the canonical owner
  of the RPC contract package (`@ployz/deploy`, or a sibling `@ployz/rpc` for
  the envelope/Status/Mesh/operation payloads). **Every RPC payload from M1
  onward is generated from live Rust types.** `DeployManifest` schema lands in
  M3, but the *pipeline and ownership* are a prerequisite of the first typed
  RPC, not of the deploy RPC — otherwise M1/M2 payloads drift exactly the way
  the roadmap warns about for `DeployManifest`.
- **Hosted installer ownership (named, not deferred).** Add: (a) the installer
  script that places the binary at `$HOME/.local/bin/ployzctl` and starts the
  system-mode daemon; (b) the NDJSON **installer-events** schema as a pinned
  contract cloud consumes; (c) an explicit owner + repo + versioning decision
  for `ployz.sh` — it is a **third deployable**, not core or cloud. Without an
  owner, `isPloyzInstalled` onboarding cannot work after any milestone.

**Done when:** `isPloyzInstalled` passes against a real curl-installed binary;
cloud can probe `Status` over SSH and read `capabilities[]`; every M1 RPC
payload and the installer-events schema are generated and pinned; JSON is
consumable by a zero-context agent.

**Depends on:** M0

### M2a — Founder/joiner provisioning over SSH + durable operation records + mesh RPCs (no overlay)

**Serves:** idempotent founder/joiner provisioning, MeshStatus/MeshReady (store
+ sync axes), MeshSelfRecord, MachineOperationList/machine-list, network-name
== org slug.

**Deliverables:**
- **Org-slug→IslandId adapter.** "Network name == org slug" is a ployz product
  concept implemented in `crates/ployz/src/adapters/polis/`, mapped onto
  `polis::IslandId`. `polis` gains no network/org API. The idempotency codes
  are produced by ployz product logic, not by the substrate.
- Idempotent `mesh init <slug>` / `mesh start <slug>` returning the exact
  `NETWORK_ALREADY_EXISTS` / `NETWORK_ALREADY_RUNNING` / `NETWORK_NOT_FOUND`
  codes (adopt-not-reinit), so `retryServerProvision` is safe.
- `machine add --identity <keyfile> <user@host>` driving an SSH join from the
  controller, wrapping the existing in-process `ployz::machine add`.
- **Durable async operation-record subsystem (its own first-class
  deliverable, not a bullet under provisioning).** A single-writer operation
  table carrying `{id,kind,network_name,targets[],status,stage,last_error,
  machine_id}`, written by the controller that drives the join, with **no
  liveness inferred into the record**. This is net-new (today `OperationId`
  is a bare newtype). Sequence the record store *before* the RPC that reads it.
- `MeshStatus` / `MeshReady` / `MeshSelfRecord` RPCs, probe-at-decision-time.
  `MeshReady` reports `store_healthy` and `sync_connected` (both from the M0
  health signal). `workload_subnet_present` is reported **honestly as `false`
  here** and is owned by M2b (see that milestone for the contract decision).
- `MachineOperationList` / `machine-list` over rpc-stdio, capability-gated and
  `UNKNOWN_COMMAND`-tolerant.

**Done when:** cloud can form a two-node Corrosion-membership cluster (founder
+ joiner) idempotently over SSH; retries are safe; cloud can read
`operations[]` and **detect an interrupted join** (hard gate, not optional);
`MeshReady` reports real store/sync health. (Note: nodes share state but do
**not** yet have an encrypted overlay — that is M2b.)

**Depends on:** M1

### M2b — Authority-island WireGuard overlay: controller + adoption + workload subnet

**Serves:** the encrypted data plane; `MeshReady.workload_subnet_present`;
controller selection that depends on a real overlay.

**Deliverables:**
- Real authority-island **WireGuard controller**: interface create, peer
  config, key management, overlay IP allocation, restart-adoption (the daemon
  restart rebuilds the same mesh without rewriting cluster truth). This is a
  net-new subsystem comparable in size to the M3 container backend; it is
  called out as its own milestone, not a sub-bullet of provisioning.
- **Workload-subnet reservation.** `workload_subnet_present` is defined to mean
  *"the WG overlay has a workload CIDR reserved on this node"* (not "a workload
  is running"). The reservation is performed here, so `MeshReady=true` becomes
  reachable after M2b for cloud's `isJoinControllerReady` gate. The semantics
  are pinned in the generated contract.

**Done when:** a second machine joins and routes to the first over the
encrypted overlay; `MeshReady.workload_subnet_present` is true once the CIDR is
reserved; a daemon restart adopts the running interface and peers without
re-keying or rewriting membership truth.

**Depends on:** M2a

### M3 — Multi-service deploy: real container backend + `DeployApply` + canonical phase contract + `coreDeployId` + single-service runtime stream

**Serves:** DeployApply (the real multi-service manifest cloud actually sends),
@ployz/deploy `DeployManifest` ownership, namespace-scoped atomic deploy,
snapshot config, plaintext env, **and** the first live observability surface.

**Deliverables:**
- **Adopt cloud's `@ployz/deploy DeployManifest` shape**
  (`{namespace,services[],volumes[]}`) as the core contract and **retire** the
  incompatible single-route `DeployManifest` in deploy/mod.rs.
- **Accept the full multi-service manifest shape from day one**, executing
  services **sequentially** under one atomic commit. (Rejecting the
  single-service slice: cloud's smallest unit is a multi-service environment
  manifest, so a single-service-only M3 would be exercisable only by core
  internal tests, never by cloud. Sequential execution keeps the slice small
  while letting cloud's real manifests apply.) M4 adds routing/ACME/stream
  breadth, not the first multi-service capability.
- **Canonical phase contract, pinned before build.** Reconcile the three phase
  models (core's real 3-axis `DeployPlan`, the prose 5-step narrative, cloud's
  `EnvironmentDeploymentPreview{commitPolicy,rollbackPolicy,advancePolicy}`)
  into **one** contract published in `@ployz/deploy`. The core's execution
  reality (per-axis domain/runtime/serving) is the source of truth; cloud's
  preview phases are defined as a deterministic projection of it. Do **not**
  ship an apply against an unstated phase shape.
- **`coreDeployId` minted from the first apply.** Even though structured-phase
  preview and cancel land in M7, every `DeployApply` returns a stable,
  coordinator-minted `coreDeployId` now (cheap), backed by a **minimal durable
  deploy-commit/correlation record** (single writer = coordinator). This is the
  one deliberate, scoped exception to the no-deploy-tables gate, and it avoids a
  backward-incompatible reshape of the apply response at M7.
- **Real local container backend** implementing an expanded `RuntimePort`:
  `start / verify / stop / inspect / cleanup / logs` **+ adoption of
  already-running instances**. The trait today has only `activate`/`verify`;
  the expansion (stop/cleanup) is designed in now so M7 cancel is a thin RPC
  over an already-cancellable executor, not a second `RuntimePort` redesign.
  The deploy executor holds cancellable handles with per-op deadlines.
- Durable container rows: namespace label, service identity, runtime identity,
  machine, image/spec digest, ports, volume refs, adoption data + local spec
  persistence for rehydration.
- `DeployApply` RPC over rpc-stdio: lock namespace → start candidate(s) →
  readiness probe → atomic commit → cleanup; returns
  `{ok,code,message,payload}` including `coreDeployId`.
- Honor `ContainerSpec` env (arbitrary plaintext; confidential **in transit
  only** — see invariant 8), command, ports, healthcheck→readiness.
- **Image pull** is part of this container backend (this is the only "build/
  image" capability the core needs — see NOT-building).
- **Minimal single-service runtime stream.** A `runtime stream --json`
  snapshot + per-instance phase for the deployed services, so cloud has live
  deploy observability **before** the full RoutingState model lands in M4.
  This also forces the `InstanceStatusRecord` phase enum
  (`Pending/Starting/Ready/Failed/Draining/Removed`) and `RuntimeWatchFrame`
  frame kinds (`snapshot|upsert|remove|error|heartbeat`) to be **pinned and
  proven against derivable container state now**, rather than asserted in M4.

**Done when:** cloud's real compiled multi-service manifest applies atomically
over a real container runtime; service/namespace views derive from container
rows; the apply returns a stable `coreDeployId`; the phase contract is
generated from live Rust types and maps 1:1 to cloud's preview fields; cloud
can observe instance phase transitions over the stream. **Explicitly NOT yet:**
services are reachable over HTTP/HTTPS (the gateway/DNS consumer is M4) — the
M3 "commit" makes containers running and observable, not externally reachable.

**Depends on:** M2b

### M4a — Routing & live observability: HTTP routing + full RoutingState + `runtime stream`

**Serves:** service reachability over plain HTTP, cloud's full live UI feed,
service-reference resolution.

**Deliverables:**
- **`RoutingState` truth model + snapshot/invalidation projection** derived
  from container/serving/membership rows. This is the foundational deliverable
  that both the gateway and the stream consume; it does not exist today
  (serving is commit rows only). Called out explicitly so it is not buried.
- Gateway/DNS projection process (adopt-on-restart): bind hostnames, route HTTP
  by host/path to `service_port`, expose the routing pointers the M3 commit
  flips. The M3 atomic commit becomes externally meaningful here.
- Full `runtime stream --json` `RuntimeWatchFrame` over
  `RoutingState{machines,revisions,releases,instances}` (extends the M3
  single-service stream to the full model).
- Multi-service routing breadth: placement (global/replicated count),
  `service_ports`, `publish`, **http** `RouteSpec` (hostnames/path), `rollout`
  = **recreate only** (atomic readiness-gated flip; blue_green deferred —
  see NOT-building), labels.
- **Documented core-assigned addressing** so cloud resolves
  `${{Service.PUBLIC_DOMAIN}}` at manifest-compile time. This is a hard
  prerequisite of reference variables; it is decided here.
- Derived service/route/namespace views from container rows (no durable
  services/routes/namespaces tables).

**Done when:** an environment of services is reachable over plain HTTP with
host/path routing; cloud's live UI is fed entirely by the stream; reference
variables resolve against the documented addressing scheme; a daemon restart
adopts the running gateway without dropping routes.

**Depends on:** M3

### M4b — Public HTTPS: real ACME HTTP-01 + TLS termination + preDeploy hook

**Serves:** Railway-parity public HTTPS, migrations.

**Deliverables:**
- Real ACME **HTTP-01** CA client behind `CertificateAuthorityPort` (today
  fake-only); certificate material/status + ACME account/order/challenge/
  attempt rows; HTTP-01 challenge routing through the M4a gateway.
- TLS termination in the gateway; an HTTPS route is **gated by readiness +
  cert usability** and is **never published unusable** (release-gate
  invariant).
- Typed **pre-deploy one-shot step** (`preDeployCommand`) run **before** the
  routing flip; non-zero exit fails the deploy. Surfaced in the generated
  schema (no field exists in `@ployz/deploy` today). Kept as its own small
  slice gated on a real migration use case rather than folded into routing.

**Done when:** a deployed service gets a real, valid public HTTPS certificate;
failed issuance/renewal is visible and never yields a live broken route;
migrations run via preDeploy before traffic flips.

**Depends on:** M4a

### M5 — Ployz-managed ZFS volumes: fresh create, attach, owner-pinned placement

**Serves:** persistent/stateful services, stateful atomic deploy, placement
correctness.

**Deliverables:**
- Real ZFS backend: dataset **create/adopt/destroy, mountpoint, quota**.
  (**Snapshot is deferred** to ship with its first consumer — clone or rollback
  — rather than ahead of need; the M5 done-criteria never required it.)
- Durable volume rows: owner machine, dataset identity, scope, quota; container
  rows reference attached volume ids.
- Fresh `volume create` + deploy-with-attach; deploy planning pins stateful
  services to volume owners.
- Reject unsafe multi-writer/multi-replica stateful shapes; volume + pool
  doctor output.
- Volume rows carry enough durable identity to add snapshot/clone/move later
  without migrating data.

**Done when:** persistent data is always under a managed, quota-bounded ZFS
dataset; a stateful service cannot start off its volume owner; rows carry
enough identity to add clone/move later without migration.

**Depends on:** M4b

### M6 — Environment cloning (cloud-side composition path) + optional ZFS fork-volume *(reframed; was a forced invariant exception)*

**Serves:** the user's headline target — cloning environments — on the cheapest
correct path.

This milestone is **split into a default path and an optimization gated on a
measured need**, because the discovery report itself states cloud's cloning is
*"cloud DB logic producing ordinary services + manifests."* A branch
environment is a fresh namespace deploy; it does not require new core
primitives to *function*, only to be *instant*.

**Default path (ships first, no new core primitive):**
- Cloning is **cloud-side manifest composition** over M3/M4 deploys + M5 fresh
  volumes, optionally **seeded from a snapshot restore** (snapshot lands here
  as the first real consumer that justifies building it). A branch env deploys
  as an ordinary namespace with fresh-or-restored volumes. No durable
  namespaces lineage table is required.

**Optimization (gated, only if instant-CoW latency proves to be a product
blocker):**
- ZFS **copy-on-write clone** behind a `fork-volume` primitive, owner-machine
  fenced — additive on the M5 volume rows by design.
- A durable single-writer `namespaces` lineage table **only if** lineage
  queries are required for promote/rollback; this is the one place the roadmap
  reverses a 1.0 cut, and it is **deferred behind a measured need** rather than
  placed on the critical path. If built, it is an owned table (owner = the
  branch/create command), not a desired-state model, and the branch compiler
  stays command-shaped (preview/apply, no reconciler).

**Done when (default):** cloud can clone an environment as a normal namespace
deploy with fresh or snapshot-restored volume data; views remain derivable;
every operation is owner-fenced and atomic. **Done when (optimization, if
triggered):** clone data is instant (CoW) and lineage is queryable for later
promote/rollback.

**Pre-M6 deploys are not retroactively branchable from lineage** — lineage
(if built) starts empty; cloning composes from current manifests, not from a
historical lineage graph.

**Depends on:** M5

### M7 — Phased preview + cancel; disposable-machine stateless drain/remove *(scoped down)*

**Serves:** multi-phase preview, deploy cancel, disposable machines —
restricted to what cloud has cited evidence for.

**Deliverables:**
- Deploy `preview` RPC returning structured phases derived from the M3
  canonical phase contract, with commit/rollback/advance semantics matching
  `EnvironmentDeploymentPreview`; `apply` advances them, keyed by the
  `coreDeployId` **already minted in M3** (no apply-response reshape needed).
- `deploy cancel <coreDeployId>` interrupting an in-flight deploy **before
  commit**, reporting **failed-before-commit vs failed-after-checkpoint**. This
  is a thin RPC over the cancellable executor + stoppable `RuntimePort`
  designed in M3.
- `machine drain` (compile service + route changes) and safe `machine remove`
  (tombstone **only after no active placement remains**; never infer liveness
  into durable truth), **constrained to stateless roles**.
- **Cross-machine stateful volume move (ZFS send/receive) is NOT in this
  milestone.** It stays deferred (as in the 1.0 roadmap) until a concrete cloud
  need exists; the open question "does cloud's MVP dispose of *stateful*
  machines?" is resolved *before* committing it, not inside committed
  deliverables. Disposable machines are stateless in the MVP.

**Done when:** cloud's phased-deploy schema/UI is backed by real core phases +
ids; cancellation reaches the daemon with failed-before/after-commit clarity; a
**stateless** machine can be drained and removed safely without disrupting the
data plane.

**Depends on:** M6 (default path) — does not depend on the M6 optimization.

## What We Are Deliberately NOT Building (and why cloud doesn't need it yet)

- **Autoscaling / self-healing loops / reconcilers** — VISION bans them; any
  cloud elasticity drives discrete commands from outside the core.
- **In-core AI / operator workflow execution** — stays in cloud/Inngest.
- **Git/GitHub build orchestration, hosted builders, git-push deploys, and any
  `build → image-digest` primitive** — fully cloud-layer. The core's only
  image responsibility is the **image-pull path in the M3 container backend**;
  cloud-hosted builds produce a pinned digest and feed it into an image-only
  manifest. (Removed the "at most a build primitive" hedge — there is no
  separate core build primitive.)
- **Blue/green and other rollout strategies** — MVP ships **recreate** (atomic
  readiness-gated flip) only; the flip is the minimum and already lands in
  M3/M4a. Additive versioning lets cloud request more later.
- **`options:{}` reserved manifest hole** — dropped from the committed
  contract. Additive versioning (already the strategy) adds real options when
  they exist; reserving an empty hole is gold-plating.
- **Secret store / sealing in core** — 100% cloud-side (AES-256-GCM). The core
  accepts arbitrary plaintext env and keeps it confidential **in transit
  (SSH)**; it is **plaintext at rest on every mesh node** because Corrosion
  replicates container rows. There is no "encrypted at rest" core promise —
  cloud's threat model must account for plaintext env on each node's disk.
- **A standalone "sync variables without redeploy" path** — a variable change
  is a manifest change and goes through `DeployApply`. If cloud requires
  variable propagation without a full redeploy, that is a *new capability* to
  scope, not an assumed one (see Open Questions).
- **Wildcard certs, DNS-01, custom cert upload, non-ACME providers** — beyond
  HTTP-01; cloud MVP does not need them.
- **TCP route proxies** — the only evidence is a `tools.md` promise, not a
  `DeployApply` payload that carries a `tcp RouteSpec` on the MVP path. Deferred
  until a real manifest carries one.
- **Multi-source branch composition, provider-native DB branches, portal /
  shared-read-only volume modes** — reserved keywords, rejected in any branch
  compiler.
- **Cross-machine ZFS send/receive volume move** — deferred until a concrete
  stateful-disposal need is proven; disposable machines are stateless in the
  MVP.
- **Durable services/routes tables as truth** — views remain derived; the only
  durable critical-path addition is the `coreDeployId` correlation record (M3).
  The `namespaces` lineage table is deferred behind a measured need (M6
  optimization).
- **A second transport** — SSH-exec of `ployzctl` + `rpc-stdio` +
  `runtime stream` remain the seam; no separate HTTP/gRPC control plane.

## Open Questions / Contract Risks

- **Installer is a third deployable.** `ployz.sh` lives in neither repo; M1
  names an owner/repo/versioning decision as a deliverable. Until that owner
  exists, `isPloyzInstalled` onboarding and the NDJSON installer-events schema
  are unfulfilled. This is a program risk, not just a contract risk.
- **Three phase models must collapse to one (resolved structurally in M3, but
  needs sign-off).** Core executes a **3-axis** `DeployPlan`
  (domain/runtime/serving, each `Current|Apply`), the prose describes a linear
  5-step pipeline, and cloud ships `commit/rollback/advance` preview phases.
  M3 pins the 3-axis reality as truth and defines cloud's preview as a
  projection of it. If instead the team wants a linear pipeline, `DeployEngine`
  must be refactored *before* M3 exposes any phase RPC. Do not ship two
  unstated phase shapes.
- **`coreDeployId` from the first apply.** Minting it in M3 (not M7) is a
  deliberate cheap insurance against a backward-incompatible apply-response
  reshape; it does promote a minimal durable deploy-commit record — a conscious,
  scoped reversal of the 1.0 no-deploy-tables gate.
- **`MeshReady.workload_subnet_present` semantics.** Pinned in M2b to mean "WG
  workload CIDR reserved on this node," making `MeshReady=true` reachable after
  M2b. Verify cloud's gate (`ready OR phase==running && store_healthy &&
  sync_connected`) has a real path to true once M2b lands; until then cloud must
  tolerate `subnet_present=false` from M2a.
- **Stringly error contract.** Cloud branches on exact `code` strings and on
  raw stderr text (`message.includes(code)`) for mesh init/start. Any rename of
  a subcommand, the envelope, or a code silently breaks cloud. Reproduce
  exactly; change only additively. The M1 generation pipeline is what keeps
  this from drifting.
- **`capabilities[]` governance.** Kept as a real wire field (cloud reads it),
  but it must be a single pinned, additively-versioned token registry that
  every milestone appends to — not per-milestone ad-hoc strings.
- **Variable/secret seam.** A variable change today requires a full
  `DeployApply` (env is only carried in the manifest). Confirm whether cloud's
  variable-management UX expects propagation without a redeploy; if so, a
  lighter env-update path is a *new* capability to scope. Document the
  redeploy-on-var-change cost either way.
- **Service-reference resolution depends on M4a addressing.** `${{Service.
  PUBLIC_DOMAIN}}` cannot resolve until the core-assigned addressing scheme is
  decided in M4a. Reference variables therefore do not work end-to-end until
  M4a, even though env injection exists in M3.
- **`InstanceStatusRecord` / `RuntimeWatchFrame` mapping.** The 6-state phase
  enum and the frame kinds are pinned and proven against derivable container
  state in **M3** (not asserted in M4). If core readiness/serving state cannot
  cleanly map onto the 6 phases, that mismatch surfaces at M3, not after the
  full RoutingState is built.
- **Cancellation gap until M7.** Cloud marks rows cancelled but cannot reach an
  in-flight 30-min `DeployApply` until M7. Because M3 designs a cancellable
  executor + stoppable `RuntimePort`, M7 cancel is thin; until M7 ships,
  ambiguous active state is a live risk cloud must surface.
- **Disposable-machine scope (resolve before M7 build).** Whether cloud's MVP
  ever disposes of *stateful* machines determines if cross-machine ZFS
  send/receive is needed at all. The MVP assumes stateless disposal; revisit
  before committing the move primitive.
- **Cloning path choice (M6).** The default path (cloud-side composition over
  fresh/snapshot-restored volumes) ships with no new core primitive. The ZFS
  CoW `fork-volume` + `namespaces` lineage table are an optimization gated on a
  measured instant-clone latency need — confirm whether the product actually
  requires instant CoW before building it.
