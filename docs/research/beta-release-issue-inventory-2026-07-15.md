# Open-work and release-assertion inventory

Snapshot: **2026-07-15T02:29:32Z**. Scope: every open issue in
[`getployz/ployz`](https://github.com/getployz/ployz/issues) and
[`getployz/ployz-dashboard`](https://github.com/getployz/ployz-dashboard/issues),
including issue bodies and comments, native parent/sub-issue relationships,
native `blocked_by` relationships, Wayfinder maps, release labels, the existing
[`v1: first release` milestone](https://github.com/getployz/ployz/milestone/1),
and explicit cross-repository issue links.

This is an evidence inventory, not a new classification. `BG` means the issue
already carried `release:beta-gate`; `NC` means it already carried
`release:needs-classification`. No open issue carried `release:post-beta` at
the snapshot. “No further claim” means the issue body/comments made no more
specific pre-release or deferral assertion; it does not mean the work belongs
on either side of the beta boundary.

## Snapshot result

- **81 open issues:** 72 in Core and 9 in Dashboard.
- **10 BG / 71 NC / 0 Post-Beta:** all 10 BG issues are the canonical beta map
  and its nine children; every other open issue remains NC.
- **One legacy milestone:** `v1: first release` is open with 3 open and 44
  closed issues. Its description says Runtime + CLI ships when a five-step
  fresh-machines acceptance script passes.
- **Six open Wayfinder maps:** four in Core and two in Dashboard. Their
  destinations do not currently agree on one
  release boundary; details are below.

## Canonical beta map and children

| Issue | Present purpose | Native parent | Open native blockers | Existing release assertion |
|---|---|---|---|---|
| [Wayfinder: Canonical Ployz beta release tracker](https://github.com/getployz/ployz/issues/509) | Canonical cross-repository classification, acceptance, and dependency map. | — | — | **BG.** Defines Beta Gate, Post-Beta, and later V1; explicitly says the hosted Core/CLI + Dashboard product is one beta and that V1 is later. |
| [Audit every open issue and existing release assertion across both repositories](https://github.com/getployz/ployz/issues/510) | Produce this evidence inventory without classifying ambiguity. | Canonical beta map | — | **BG.** Explicit beta-map prerequisite. |
| [Fix the combined hosted-beta product and acceptance contract](https://github.com/getployz/ployz/issues/511) | Settle user-visible beta promises, evidence, host matrix, and exclusions. | Canonical beta map | — | **BG.** Explicit hosted-beta contract. |
| [Specify the beta ZFS Volume Driver and managed-pool lifecycle](https://github.com/getployz/ployz/issues/512) | Specify ZFS driver, managed pool/dataset modes, lifecycle, failures, and acceptance. | Canonical beta map | — | **BG.** Explicit beta architecture; snapshot/clone APIs, Btrfs, replication, and cross-machine moves are explicitly Post-Beta. |
| [Specify the core Build Adapter contract and multi-architecture output](https://github.com/getployz/ployz/issues/513) | Specify bounded Dockerfile/Railpack builds and amd64/arm64 OCI-index output. | Canonical beta map | — | **BG.** Explicit beta architecture. |
| [Audit hosted Dashboard push-to-build-to-deploy readiness](https://github.com/getployz/ployz/issues/514) | Audit the current Dashboard-to-core hosted deployment journey and identify implementation-sized gaps. | Canonical beta map | — | **BG.** Explicit hosted-beta audit; asks not to decide unrelated Post-Beta features. |
| [Classify every open core and Dashboard issue against the beta boundary](https://github.com/getployz/ployz/issues/515) | Give every open issue exactly one canonical release label and an evidence-backed rationale. | Canonical beta map | [inventory](https://github.com/getployz/ployz/issues/510), [contract](https://github.com/getployz/ployz/issues/511), [ZFS](https://github.com/getployz/ployz/issues/512), [Build Adapter](https://github.com/getployz/ployz/issues/513), [Dashboard audit](https://github.com/getployz/ployz/issues/514) | **BG.** Explicit beta classification gate. |
| [Reconcile the legacy V1 milestone and existing Wayfinder maps](https://github.com/getployz/ployz/issues/516) | Retire/rename misleading legacy release artifacts and repair stale map/parent relationships. | Canonical beta map | [classification](https://github.com/getployz/ployz/issues/515) | **BG.** Explicitly separates beta from former V1 terminology. |
| [Specify the sealed cross-repository hosted-beta acceptance fixture](https://github.com/getployz/ployz/issues/517) | Specify the final repeatable GitHub/build/deploy/ZFS/multi-arch/failure acceptance fixture. | Canonical beta map | [contract](https://github.com/getployz/ployz/issues/511), [ZFS](https://github.com/getployz/ployz/issues/512), [Build Adapter](https://github.com/getployz/ployz/issues/513), [Dashboard audit](https://github.com/getployz/ployz/issues/514), [classification](https://github.com/getployz/ployz/issues/515) | **BG.** Explicit combined hosted-beta acceptance gate. |
| [Publish the dependency-wired Beta Gate implementation graph](https://github.com/getployz/ployz/issues/518) | Publish the implementation-ready cross-repository graph without implementing it. | Canonical beta map | [reconciliation](https://github.com/getployz/ployz/issues/516), [acceptance fixture](https://github.com/getployz/ployz/issues/517) | **BG.** Explicit final beta-map planning gate. |

## Core architecture-deepening graph

These issues form a native dependency graph but have no native parent or
Wayfinder map. Every row is **NC with no further release claim** unless noted.

| Issue | Present purpose | Open native blockers |
|---|---|---|
| [Establish the canonical Operation module](https://github.com/getployz/ployz/issues/477) | Give operation domain types and behavior one canonical module. | — |
| [Give Intent and Machine canonical modules](https://github.com/getployz/ployz/issues/478) | Establish canonical homes for Intent and Machine concepts. | — |
| [Establish the canonical Network module](https://github.com/getployz/ployz/issues/479) | Consolidate network domain behavior behind one module. | — |
| [Rename and deepen the Certificate module](https://github.com/getployz/ployz/issues/480) | Give certificate behavior a coherent, deeper interface. | — |
| [Deepen the Deploy module](https://github.com/getployz/ployz/issues/481) | Consolidate deploy policy behind a deeper module boundary. | — |
| [Deepen the Install module](https://github.com/getployz/ployz/issues/482) | Consolidate installation policy behind a deeper module boundary. | — |
| [Expand NATS-owned concrete interfaces](https://github.com/getployz/ployz/issues/483) | Move concrete NATS ownership behind appropriate interfaces. | — |
| [Put the complete eBPF subsystem under one root](https://github.com/getployz/ployz/issues/484) | Consolidate the eBPF subsystem under one root module. | — |
| [Give recovery and testimony honest shared homes](https://github.com/getployz/ployz/issues/485) | Place recovery/testimony concepts at honest shared seams. | [epoch bug](https://github.com/getployz/ployz/issues/474), [Operation](https://github.com/getployz/ployz/issues/477), [Intent/Machine](https://github.com/getployz/ployz/issues/478), [Network](https://github.com/getployz/ployz/issues/479) |
| [Put execution adapters behind the Machine role](https://github.com/getployz/ployz/issues/486) | Hide execution adapters behind the Machine-role boundary. | [Intent/Machine](https://github.com/getployz/ployz/issues/478), [Network](https://github.com/getployz/ployz/issues/479) |
| [Give Control-owned state and projections one module](https://github.com/getployz/ployz/issues/487) | Consolidate Control-owned state and projections. | [Operation](https://github.com/getployz/ployz/issues/477), [Intent/Machine](https://github.com/getployz/ployz/issues/478), [Network](https://github.com/getployz/ployz/issues/479), [Certificate](https://github.com/getployz/ployz/issues/480), [recovery/testimony](https://github.com/getployz/ployz/issues/485) |
| [Put operator RPC and operations under Control](https://github.com/getployz/ployz/issues/488) | Move operator RPC and operation ownership beneath Control. | [Operation](https://github.com/getployz/ployz/issues/477), [Intent/Machine](https://github.com/getployz/ployz/issues/478), [Network](https://github.com/getployz/ployz/issues/479), [Certificate](https://github.com/getployz/ployz/issues/480), [Control state](https://github.com/getployz/ployz/issues/487) |
| [Break sequencer and role-RPC dependency cycles](https://github.com/getployz/ployz/issues/489) | Remove dependency cycles between sequencing and role RPC. | [Machine adapters](https://github.com/getployz/ployz/issues/486), [Control RPC](https://github.com/getployz/ployz/issues/488) |
| [Consolidate Machine, Gateway, and DNS test targets](https://github.com/getployz/ployz/issues/490) | Reorganize role-focused test targets. | [obsolete DNS projection](https://github.com/getployz/ployz/issues/475), [recovery/testimony](https://github.com/getployz/ployz/issues/485), [Machine adapters](https://github.com/getployz/ployz/issues/486) |
| [Consolidate Control and daemon-lifecycle test targets](https://github.com/getployz/ployz/issues/491) | Reorganize Control and lifecycle test targets. | [Control state](https://github.com/getployz/ployz/issues/487), [Control RPC](https://github.com/getployz/ployz/issues/488), [cycle break](https://github.com/getployz/ployz/issues/489) |
| [Close the daemon's accidental public interface](https://github.com/getployz/ployz/issues/492) | Reduce accidental daemon API exposure. | [black-box E2E](https://github.com/getployz/ployz/issues/476), [role tests](https://github.com/getployz/ployz/issues/490), [Control tests](https://github.com/getployz/ployz/issues/491) |
| [Migrate NATS configuration and authority consumers](https://github.com/getployz/ployz/issues/493) | Move configuration/authority consumers to canonical NATS interfaces. | [NATS interfaces](https://github.com/getployz/ployz/issues/483), [Control state](https://github.com/getployz/ployz/issues/487) |
| [Migrate NATS subject and endpoint consumers](https://github.com/getployz/ployz/issues/494) | Move subject/endpoint consumers to canonical NATS interfaces. | [NATS interfaces](https://github.com/getployz/ployz/issues/483), [Control RPC](https://github.com/getployz/ployz/issues/488) |
| [Contract the obsolete Core NATS surface](https://github.com/getployz/ployz/issues/495) | Remove obsolete Core NATS surface after consumers migrate. | [configuration migration](https://github.com/getployz/ployz/issues/493), [subject migration](https://github.com/getployz/ployz/issues/494) |
| [Migrate SDK and shared tests to canonical Core paths](https://github.com/getployz/ployz/issues/496) | Update SDK/tests to canonical Core module paths. | [Operation](https://github.com/getployz/ployz/issues/477), [Intent/Machine](https://github.com/getployz/ployz/issues/478), [Network](https://github.com/getployz/ployz/issues/479), [Certificate](https://github.com/getployz/ployz/issues/480) |
| [Give CLI connection and execution support one module](https://github.com/getployz/ployz/issues/497) | Consolidate CLI connection/execution support. | [Operation](https://github.com/getployz/ployz/issues/477), [NATS interfaces](https://github.com/getployz/ployz/issues/483) |
| [Put Compose and Deploy behavior together](https://github.com/getployz/ployz/issues/498) | Group Compose and Deploy CLI behavior by feature. | [Deploy](https://github.com/getployz/ployz/issues/481), [CLI support](https://github.com/getployz/ployz/issues/497) |
| [Put Machine installation and lifecycle behavior together](https://github.com/getployz/ployz/issues/499) | Group Machine install/lifecycle CLI behavior. | [Intent/Machine](https://github.com/getployz/ployz/issues/478), [Install](https://github.com/getployz/ployz/issues/482), [CLI support](https://github.com/getployz/ployz/issues/497) |
| [Put Network and Operation commands in feature modules](https://github.com/getployz/ployz/issues/500) | Group Network and Operation CLI commands by feature. | [Operation](https://github.com/getployz/ployz/issues/477), [Network](https://github.com/getployz/ployz/issues/479), [CLI support](https://github.com/getployz/ployz/issues/497) |
| [Finish the vertical CLI tree](https://github.com/getployz/ployz/issues/501) | Complete the feature-oriented CLI module tree. | [Certificate](https://github.com/getployz/ployz/issues/480), [Compose/Deploy](https://github.com/getployz/ployz/issues/498), [Machine CLI](https://github.com/getployz/ployz/issues/499), [Network/Operation CLI](https://github.com/getployz/ployz/issues/500) |
| [Deepen the Host Runner plan module](https://github.com/getployz/ployz/issues/502) | Consolidate Host Runner planning behind a deeper interface. | [Operation](https://github.com/getployz/ployz/issues/477), [Install](https://github.com/getployz/ployz/issues/482) |
| [Group Host Runner execution and supervision adapters](https://github.com/getployz/ployz/issues/503) | Group Host Runner execution/supervision adapters. | [Host Runner plan](https://github.com/getployz/ployz/issues/502) |
| [Group bootstrap, join, and substrate lifecycles](https://github.com/getployz/ployz/issues/504) | Consolidate bootstrap, join, and substrate lifecycles. | [Intent/Machine](https://github.com/getployz/ployz/issues/478), [Network](https://github.com/getployz/ployz/issues/479), [Install](https://github.com/getployz/ployz/issues/482), [Host Runner plan](https://github.com/getployz/ployz/issues/502), [Runner adapters](https://github.com/getployz/ployz/issues/503) |
| [Group core recovery and close the Host Runner interface](https://github.com/getployz/ployz/issues/505) | Group recovery and seal the Host Runner boundary. | [Certificate](https://github.com/getployz/ployz/issues/480), [NATS config](https://github.com/getployz/ployz/issues/493), [Host Runner plan](https://github.com/getployz/ployz/issues/502), [Runner adapters](https://github.com/getployz/ployz/issues/503), [lifecycles](https://github.com/getployz/ployz/issues/504) |
| [Put test-only packages under testing/](https://github.com/getployz/ployz/issues/506) | Move test-only packages under a testing root. | [black-box E2E](https://github.com/getployz/ployz/issues/476), [SDK/tests migration](https://github.com/getployz/ployz/issues/496), [recovery boundary](https://github.com/getployz/ployz/issues/505) |
| [Remove legacy Core module aliases](https://github.com/getployz/ployz/issues/507) | Remove aliases after module consumers migrate. | [daemon API](https://github.com/getployz/ployz/issues/492), [NATS contraction](https://github.com/getployz/ployz/issues/495), [SDK/tests migration](https://github.com/getployz/ployz/issues/496), [CLI tree](https://github.com/getployz/ployz/issues/501), [recovery boundary](https://github.com/getployz/ployz/issues/505) |
| [Publish the contributor code map](https://github.com/getployz/ployz/issues/508) | Publish contributor-facing ownership/navigation documentation. | [eBPF root](https://github.com/getployz/ployz/issues/484), [testing root](https://github.com/getployz/ployz/issues/506), [alias removal](https://github.com/getployz/ployz/issues/507) |

## Other Core maps and their open work

| Issue | Present purpose | Native parent | Open native blockers | Existing release assertion |
|---|---|---|---|---|
| [Wayfinder: Separate ingress endpoints, managed DNS, hostnames, and certificates](https://github.com/getployz/ployz/issues/462) | Planning map separating canonical ingress projection from managed DNS, hostname, and certificate concerns. | — | — | **NC;** no further release claim. |
| [Specify ingress DNS TTL and convergence guidance](https://github.com/getployz/ployz/issues/468) | Decide TTL and convergence guidance for ingress-DNS consumers. | Ingress/DNS map | — (its recorded native blocker is closed) | **NC;** no further release claim. |
| [Wayfinder: Operation-aware CLI presentation](https://github.com/getployz/ployz/issues/435) | Planning map for attended-terminal operation presentation and stable automation output. | — | — | **NC;** no further release claim. |
| [Design the renderer ownership seam, visual vocabulary, and preview harness](https://github.com/getployz/ployz/issues/457) | Prototype renderer ownership, vocabulary, and preview seam. | CLI presentation map | — | **NC;** no further release claim. |
| [Draw the renderer implementation packet boundaries](https://github.com/getployz/ployz/issues/458) | Turn settled renderer design into implementation packets. | CLI presentation map | [renderer design](https://github.com/getployz/ployz/issues/457) | **NC;** no further release claim. |
| [Wayfinder: Real-host cross-machine acceptance (beta gate)](https://github.com/getployz/ployz/issues/384) | Make a mixed-architecture two-host install/join/deploy/HTTPS/routing/restart run repeatably green. | — | — | **NC + legacy milestone.** Explicitly “gates the v1 beta,” conflicting with the canonical map’s later-V1 terminology. |
| [Repeatable real-host acceptance harness — runbook + script](https://github.com/getployz/ployz/issues/391) | Produce and run the durable mixed-host acceptance harness. | Real-host map | [Ubuntu tcx](https://github.com/getployz/ployz/issues/442), [Rocky 8 glibc](https://github.com/getployz/ployz/issues/443), [CentOS NAT](https://github.com/getployz/ployz/issues/444), [Vultr SSH](https://github.com/getployz/ployz/issues/445) | **NC + legacy milestone;** inherits the map’s explicit v1-beta gate. |
| [CLI: add `ployz --version` (currently errors)](https://github.com/getployz/ployz/issues/405) | Add a working CLI version flag. | Real-host map | — | **NC;** inherited by a map that explicitly gates v1 beta. |
| [CLI: `init` and `machine init` help show wrong description](https://github.com/getployz/ployz/issues/407) | Correct init command help. | Real-host map | — | **NC;** inherited v1-beta map claim. |
| [A managed-lease operation is left `failed` in `ops list`](https://github.com/getployz/ployz/issues/408) | Diagnose stale failed managed-lease evidence. | Real-host map | — | **NC;** inherited v1-beta map claim. |
| [CLI: `ls`==`service list` and `inspect`==`service inspect` are duplicates](https://github.com/getployz/ployz/issues/409) | Decide/remove duplicate CLI command shapes. | Real-host map | — | **NC;** inherited v1-beta map claim. |
| [CLI: output formatting is inconsistent (only `volume list` is tabular)](https://github.com/getployz/ployz/issues/410) | Decide consistent CLI output formatting. | Real-host map | — | **NC;** inherited v1-beta map claim. |
| [Dataplane/internal-DNS ~30s warmup race after deploy](https://github.com/getployz/ployz/issues/411) | Diagnose the post-deploy dataplane/DNS readiness race. | Real-host map | — | **NC;** inherited v1-beta map claim. |
| [CLI: `compose check` is silent on success](https://github.com/getployz/ployz/issues/412) | Add an explicit success result to Compose validation. | Real-host map | — | **NC;** inherited v1-beta map claim. |
| [Interrupted core operations need typed terminal evidence](https://github.com/getployz/ployz/issues/433) | Ensure interrupted core operations terminate with typed evidence. | Real-host map | — | **NC;** inherited v1-beta map claim. |
| [Ubuntu 22.04: machine add fails when tcx ingress attachment does not persist](https://github.com/getployz/ployz/issues/442) | Fix/diagnose tcx attachment persistence on Ubuntu 22.04. | Real-host map | — | **NC;** inherited v1-beta map claim. |
| [Rocky Linux 8: release binary requires newer glibc than 2.28](https://github.com/getployz/ployz/issues/443) | Restore release-binary compatibility with Rocky 8 glibc. | Real-host map | — | **NC;** inherited v1-beta map claim. |
| [CentOS Stream 10: Docker fails to start because addrtype NAT rule cannot load](https://github.com/getployz/ployz/issues/444) | Handle Docker/NAT startup failure on CentOS Stream 10. | Real-host map | — | **NC;** inherited v1-beta map claim. |
| [Vultr Rocky Linux 10 image never becomes reachable over SSH](https://github.com/getployz/ployz/issues/445) | Determine whether/how the Vultr Rocky 10 host can enter acceptance. | Real-host map | — | **NC;** inherited v1-beta map claim. |

## Remaining open Core issues

| Issue | Present purpose | Parent/map | Open blockers | Existing release assertion |
|---|---|---|---|---|
| [Prevent intent mirror epoch regression across ployzd role processes](https://github.com/getployz/ployz/issues/474) | Fix cross-process intent-mirror epoch regression. | **Closed** former first-release map [Ship Ployz as an Uncloud-like product](https://github.com/getployz/ployz/issues/264) | — | **NC;** stale parent connects it to the former first-release plan. |
| [Remove the obsolete Route DNS projection](https://github.com/getployz/ployz/issues/475) | Remove an obsolete DNS projection after the new ingress model. | — | — | **NC;** no further release claim. |
| [Keep system E2E tests black-box](https://github.com/getployz/ployz/issues/476) | Prevent system E2E tests from coupling to internals. | — | — | **NC;** no further release claim. |
| [Allow certificate issuance to proceed concurrently per hostname](https://github.com/getployz/ployz/issues/437) | Permit independent hostname certificate issuance concurrency. | — | — | **NC;** no further release claim. |
| [Gateway restarts should use Pingora lame-duck graceful upgrades](https://github.com/getployz/ployz/issues/432) | Make gateway restarts graceful through Pingora’s upgrade mode. | — | — | **NC.** Destination explicitly says Post-v1; the bounded SIGTERM fix is the v1 scope and is out of this issue. |
| [Alpine bootstrap installs a glibc-linked ployz binary that musl cannot execute](https://github.com/getployz/ployz/issues/422) | Fix or bound Alpine/musl bootstrap compatibility. | — | — | **NC;** no further release claim. |
| [Telemetry export seam: export operation, managed Vector shipper, local metrics endpoint](https://github.com/getployz/ployz/issues/371) | Add the core telemetry-export operation, per-machine Vector lifecycle, metrics endpoint, and testimony. | — | — | **NC.** Body explicitly says Cloud v1 needs it; links Dashboard’s closed Cloud-v1 decisions. |
| [Core build op: authenticated git fetch → Dockerfile/Railpack build → PushedToSeed image index](https://github.com/getployz/ployz/issues/370) | Add core git build operation, native multi-arch fanout, and OCI-index result. | — | — | **NC.** Body explicitly puts Dockerfile and Railpack in v1 and hosted builders post-v1. |
| [Runtime: ulimits and shm_size on ContainerRuntimeSpec](https://github.com/getployz/ployz/issues/369) | Add runtime-only container options required by the Sentry template proof. | — | — | **NC.** Tied to the Application Templates/Sentry proof, while the canonical beta map explicitly says Application Templates are Post-Beta. |
| [Route protection: Password variant with inline PHC verifier](https://github.com/getployz/ployz/issues/366) | Add password route protection without a core secret store. | — | — | **NC.** Linked Cloud-v1 decisions make Password a v1 preset; the body explicitly defers a core-native secret store until post-v1. |
| [Spec: Ployz v1 first release](https://github.com/getployz/ployz/issues/292) | Legacy Runtime + CLI implementation spec and five-step ship criterion. | — | — | **NC + legacy milestone.** Explicitly defines/shipping “Ployz v1”; excludes Cloud product work and core builds, contradicting the canonical combined beta and required core Build Adapter. |

## Dashboard open issues

| Issue | Present purpose | Native parent | Open native blockers | Existing release assertion |
|---|---|---|---|---|
| [Wayfinder map: Ployz Cloud v1 readiness](https://github.com/getployz/ployz-dashboard/issues/66) | Produce a Cloud-v1 spec and implementation tickets covering hosted workflows. | — | — | **NC.** Explicit Cloud-v1 surface includes hosted push-to-deploy, onboarding, routes, ops, billing, telemetry, and more; this predates the canonical combined beta terminology. |
| [Wayfinder map: Ployz Application Catalog and Templates](https://github.com/getployz/ployz-dashboard/issues/83) | Decide a curated nested template/catalog product, proven by Sentry install and upgrade. | — | — | **NC.** Inherits Cloud-v1 decisions and is required by the old self-hosted Cloud upgrade story, but the canonical beta map explicitly classifies Application Templates and self-hosted Dashboard packaging as Post-Beta. |
| [Decide the Template Release definition format and authoring workflow](https://github.com/getployz/ployz-dashboard/issues/89) | Decide immutable release representation and first-party author/review/publish workflow. | Templates map | — | **NC;** falls under the map whose capability the canonical beta map calls Post-Beta. |
| [Decide Template Upgrade execution semantics and failure states](https://github.com/getployz/ployz-dashboard/issues/90) | Decide ordered upgrade steps, stateful handling, recovery evidence, and rollback/backup policy. | Templates map | — | **NC;** same template Post-Beta tension; old Cloud-v1 map also delegates self-hosted Cloud upgrade semantics here. |
| [Decide the catalog API and CLI search/inspect contract](https://github.com/getployz/ployz-dashboard/issues/91) | Decide read-only authenticated catalog API and CLI output. | Templates map | — (its recorded native blocker is closed) | **NC;** template Post-Beta tension. |
| [Prototype the catalog, install, and upgrade UX](https://github.com/getployz/ployz-dashboard/issues/92) | Prototype catalog browse, install, managed-resource, diff, upgrade, and failure flows. | Templates map | — | **NC;** template Post-Beta tension. |
| [Decide the Jobs / run-to-completion primitive shape](https://github.com/getployz/ployz-dashboard/issues/95) | Decide whether one-shot Jobs live in Cloud workflow, core, or both and define completion/retry semantics. | Templates map | — | **NC.** Body explicitly says it does **not** block the first Sentry acceptance; no definitive beta/post-beta classification is stated. |
| [Wire first-party telemetry (Sentry + PostHog) into the Cloud web app](https://github.com/getployz/ployz-dashboard/issues/99) | Implement minimal errors and activation-event telemetry with opt-out. | — | — | **NC.** Explicitly decided in the Cloud-v1 map; “no exhaustive tracking in v1.” |
| [Remove AWS/Hetzner provisioning and remaining legacy server schema](https://github.com/getployz/ployz-dashboard/issues/103) | Delete provider-era provisioning and keep Cloud Bootstrap + Runtime Lens as the only server path. | — | — | **NC;** no further release claim. |

## Cross-repository links in open issue evidence

GitHub does not expose a native cross-repository parent or dependency edge for
these issues. The following are body/comment links and therefore evidence of
coordination, not tracker-enforced ordering:

- Core [Password route protection](https://github.com/getployz/ployz/issues/366)
  points to Dashboard’s closed route/domain and secret-material decisions.
- Core [`ulimits`/`shm_size`](https://github.com/getployz/ployz/issues/369)
  points to Dashboard’s closed Sentry boundary classification.
- Core [build operation](https://github.com/getployz/ployz/issues/370) points to
  Dashboard’s closed Cloud-v1 and push-to-deploy decisions.
- Core [telemetry export](https://github.com/getployz/ployz/issues/371) points
  to Dashboard’s closed connection and customer-monitoring decisions.
- Dashboard [Jobs](https://github.com/getployz/ployz-dashboard/issues/95) points
  to the closed Core dependency-condition issue.
- Dashboard [Templates](https://github.com/getployz/ployz-dashboard/issues/83)
  points to open Core [`ulimits`/`shm_size`](https://github.com/getployz/ployz/issues/369)
  and several closed cross-repository prerequisites.
- Dashboard [Cloud v1](https://github.com/getployz/ployz-dashboard/issues/66)
  points to open Core [Password route protection](https://github.com/getployz/ployz/issues/366)
  and [build operation](https://github.com/getployz/ployz/issues/370), plus
  closed Core seams. These are not native blockers.
- Dashboard [Cloud telemetry](https://github.com/getployz/ployz-dashboard/issues/99)
  calls itself companion work to Core/CLI telemetry but does not link a concrete
  Core issue.

## Contradictions and work without a release owner

These are facts for the classification/reconciliation tickets, not resolutions:

1. **Beta versus V1:** the canonical map says beta is the combined hosted
   Core/CLI + Dashboard release and V1 comes later. The open legacy milestone,
   Core spec, real-host map, and Dashboard Cloud-v1 map still use V1 as the
   first/pre-release destination.
2. **Core build ownership:** the legacy Core spec says the core never builds
   and v1 ships client-orchestrated local builds. The canonical beta map and
   open Build Adapter ticket require a core Dockerfile/Railpack build operation
   for beta; the older open build issue also explicitly calls both builders v1,
   remains unparented, and overlaps the Build Adapter planning scope.
3. **Templates/self-hosting:** the canonical map explicitly puts Application
   Templates and self-hosted Dashboard packaging Post-Beta. The open Templates
   map and five open children remain NC, while the old Cloud-v1 map makes the
   Templates upgrade decision part of self-hosted Cloud’s v1 upgrade story.
4. **Explicit deferral is not reflected by the label:** the gateway lame-duck
   issue explicitly calls its destination Post-v1 but still carries NC.
5. **Legacy milestone is partial:** only the Core spec, real-host map, and
   acceptance harness are attached. It cannot represent the canonical combined
   hosted beta, and its milestone description still asserts the old Runtime +
   CLI five-step boundary.
6. **Release labels are not settled classifications:** all 71 non-canonical-map
   issues carry NC, even issues whose bodies/maps explicitly claim v1/beta or
   explicit deferral. No issue carries Post-Beta despite the canonical map’s
   explicit Post-Beta statements.
7. **Stale and absent ownership:** the intent-mirror epoch bug remains a native
   child of a closed first-release map. The 32-issue architecture-deepening
   graph, six standalone Core issues, the legacy Core spec, four cross-repo
   Core seams, two Dashboard standalone tasks, and both noncanonical Dashboard
   maps have no native parent under the canonical beta tracker.
8. **Cross-repository ordering is prose-only:** every Core↔Dashboard link found
   is an issue-body/comment pointer, not a native dependency. The final graph
   will need reciprocal evidence or another explicit convention wherever GitHub
   cannot enforce cross-repository order.

## Data method

Only first-party GitHub repository data was used. The snapshot was collected
with authenticated `gh` calls:

```text
gh api --paginate repos/getployz/{repo}/issues?state=open&per_page=100
gh issue list --repo getployz/{repo} --state open --limit 100 \
  --json number,title,body,comments,labels,milestone,assignees,url
gh api repos/getployz/{repo}/issues/{number}/parent
gh api repos/getployz/{repo}/issues/{number}/sub_issues
gh api repos/getployz/{repo}/issues/{number}/dependencies/blocked_by
gh api repos/getployz/ployz/milestones/1
```

Pull requests returned by the REST issues endpoint were excluded. Parent and
dependency calls were run for every one of the 81 open issues. Closed native
blockers were inspected but omitted from the “Open native blockers” column;
where that distinction matters, the row says the recorded blocker is closed.
Cross-repository references were extracted from every open issue body and
comment and then checked against the linked first-party GitHub issue.
