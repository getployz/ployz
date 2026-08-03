# Hosted Dashboard push-to-build-to-deploy readiness

Research for [Audit hosted Dashboard push-to-build-to-deploy readiness](https://github.com/getployz/ployz/issues/514), audited 2026-07-15 against Ployz `be608595de4a3afa1ed62cb7004fd1f0c3a4a77e` and canonical Dashboard `origin/main` at `5062f2891978d12fc78705b39966a2c5cb8ed88d`. The sibling checkout's older `feat/dev` branch was not treated as product HEAD.

## Verdict

Dashboard already has a credible image-deploy and runtime-observation foundation: SDK alpha.62, `deploy.reserve` then `deploy.submit`, deploy-scoped credentials, full namespace payload compilation, environment snapshots, declared and automatic routes, streamed Rust runtime snapshots, and interactive Cloud Bootstrap. It cannot yet complete the hosted beta journey because Git sources deliberately fail before deploy, GitHub push/check events are ignored, no Build Record or build workflow exists, and core operation evidence is reduced to status polling plus one synthetic phase without deliberate retry.

This is two core build packets plus a bounded set of Cloud workflow/evidence seams, not a Dashboard rewrite. Cloud owns GitHub knowledge, durable product workflow rows, fan-in, retry decisions, and history; core owns bounded build/deploy operations and runtime evidence. That boundary follows the [runtime vision](../../VISION.md#cloud-relationship), Dashboard's [Cloud Lens](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/CONTEXT.md#cloud-lens), the [canonical beta map](https://github.com/getployz/ployz/issues/509), and the settled [Build Adapter contract](https://github.com/getployz/ployz/issues/513#issuecomment-4976478211).

## Canonical beta contract

- The hosted beta must prove machine onboarding, GitHub push-to-build-to-deploy, public HTTPS, visible operation evidence with deliberate retry, mixed amd64/arm64 output, and volume-preserving upgrades ([beta map](https://github.com/getployz/ployz/issues/509)).
- `build.submit` is one bounded core operation. Cloud supplies an exact commit and short-lived generic git credential, selects declared platforms, watches the operation, stores its Build Record, and separately submits deploy. Dockerfile and Railpack are closed adapter variants; all requested platforms must succeed before core returns one receipt ([Build Adapter resolution](https://github.com/getployz/ployz/issues/513#issuecomment-4976478211)).
- Cloud is workflow authority, not runtime authority. Core operations are mortal runtime records; Cloud stores only durable product rows it owns ([vision](../../VISION.md#cloud-relationship), [operations ADR](../adr/0003-operations-are-informational-records-not-workflows.md)).
- Route Bindings are durable core intent, automatic hostname labels are caller-supplied without silent collision rewriting, and public DNS publication is external to the cluster DNS role ([glossary](../../CONTEXT.md#route-binding), [DNS ADR](../adr/0034-public-ingress-dns-is-external.md)).

## Readiness matrix

| Seam | What exists on Dashboard `origin/main` | Beta-blocking gap |
| --- | --- | --- |
| GitHub App | Signed installation and repository webhooks, cached repositories, branch lookup, reconciliation, and short-lived installation tokens ([API](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/models/github/github.api.ts), [webhook](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/routes/api/github/webhook.ts#L18-L93)). | The webhook handles only `installation` and `installation_repositories`. There is no idempotent `push`/`check_suite` ingestion, exact-SHA and changed-path fan-in, or service-to-installation ownership lookup. |
| Service source | Git source stores repository, branch, `autoDeploy`, and `waitForCi`; service build config stores builder, Dockerfile path, and watch paths ([schema](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/db/schema.ts#L245-L267), [build fields](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/db/schema.ts#L840-L883)). | These fields are inert. Freeze adapter-specific configuration against the core contract and use stable installation/repository identity rather than mutable `owner/name` alone. |
| Build workflow and records | None: no build endpoint in the current SDK and no Dashboard Build Record, platform result, event, receipt, reuse decision, or deploy join ([core endpoints](../../packages/ployz-sdk/src/generated.ts#L921-L945)). | Add durable Cloud build rows and a resumable Inngest workflow owning the core operation id, source/config fingerprint, per-platform evidence, terminal receipt, reuse decision, and deployment linkage. |
| Git-to-build/deploy | Manual Apply snapshots a whole environment. | Git sources explicitly fail compilation because the build pipeline is not enabled ([compiler](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/models/services/environment-deployments.ts#L282-L285)). There is no token handoff, `build.submit`, all-builds-success barrier, receipt projection, or exact-commit changeset. |
| Changeset atomicity | Image deploy compiles full runtime command, env, mounts, health, restart, resources, and routes into one namespace payload ([compiler](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/models/services/environment-deployments.ts#L147-L322)). | A push-generated changeset must freeze exact SHA, adapter config, Build Record ids and receipts; build only affected Git services; reuse unchanged receipts; and submit the namespace once only after every required build succeeds. |
| SDK/core deploy contract | Dashboard pins `@ployz/sdk@0.0.2-alpha.62`, reserves then submits using deploy-scoped credentials, persists the operation id, and resumes a committed reservation ([package](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/package.json#L39), [submit flow](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/models/services/environment-deployments.server.ts#L423-L521)). | Deploy is current. The blocker is the not-yet-implemented core build contract and its generated SDK surface; add a cross-repo fixture when that surface lands so receipt projection cannot drift. |
| Operation evidence and retry | Dashboard polls `ops.status` once per second for up to ten minutes ([polling](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/models/services/environment-deployments.server.ts#L262-L290)). | It never pages `ops.watch`; the database/UI synthesizes one Deploy phase, does not retain typed events/failures, and offers no retry of frozen input ([phase model](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/models/services/environment-deployments.ts#L89-L145), [row UI](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/components/deployment-row.tsx#L130-L251)). |
| Runtime observation | A single `watchRuntime` SSE path streams current Rust runtime snapshots, fans them into collections, and retains the last view during outage ([client](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/models/runtime/runtime-client.server.ts#L33-L71), [event stream](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/models/runtime/runtime-events.server.ts#L23-L160), [collection](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/models/runtime/runtime.collection.ts#L115-L152)). | Runtime observation is sufficiently current for beta. Operation Activity/log evidence remains a separate missing surface, not a reason to replace runtime streaming. |
| Routes and domains | Service schema stores declared routes and a managed hostname; the compiler maps concrete and automatic hostnames into deploy input ([schema](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/db/schema.ts#L245-L267), [compiler](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/models/services/environment-deployments.ts#L147-L184)). | The managed automatic-hostname path is plausible for beta, but Cloud lacks custom-domain ownership, DNS instructions/verification, certificate lifecycle, and route-specific operation evidence. Do not claim custom domains are beta-ready unless the combined contract requires them. |
| Namespace identity | Environments persist a human namespace, and deploy/runtime wiring otherwise uses current contracts. | Deploy compiles the environment UUID as `namespace_id` ([compiler](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/models/services/environment-deployments.ts#L319-L321)), conflicting with the closed frozen `<project-slug>-<env-slug>` org-wide decision ([decision](https://github.com/getployz/ployz-dashboard/issues/69)). Freeze one canonical identity before Build Records and URLs persist it. |
| Machine onboarding | Add Server shows `curl … && sudo ployz host bootstrap cloud`; session, redemption, founder/joiner, current `machine.add`, and runtime display work ([route](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/routes/_protected/cloud/$organizationSlug/_org/servers/new.tsx#L12-L50), [bridge](https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/models/servers/cloud-bootstrap-bridge.server.ts#L138-L190)). | Interactive onboarding meets the map as written. A user-facing Cloud Bootstrap Invite/token mint flow is needed only if the combined beta contract requires noninteractive onboarding. Dashboard issue [#103](https://github.com/getployz/ployz-dashboard/issues/103) already owns deletion of legacy provider/SSH code. |

## Legacy assumptions to remove or supersede

1. **GitHub installation support means push-to-deploy exists.** Current handlers intentionally ignore push/check events.
2. **Stored build fields constitute a build workflow.** Builder, Dockerfile path, and watch paths are inert until exact-SHA orchestration, records, and core submission exist.
3. **Deployment phase rows are core evidence.** They are a synthetic local phase, not retained core events, warnings, partial outcomes, or typed failures.
4. **A configured Git source is deployable.** It still fails explicitly in the compiler; only image sources proceed.
5. **Managed automatic hostname readiness implies custom-domain readiness.** Custom domains additionally need ownership, DNS instructions/verification, certificate lifecycle, and evidence.
6. **The old managed lease apex is the current route contract.** Current core separates Automatic Hostname Namespace/Label and Ployz DNS Target, never silently rewrites collisions, and leaves public DNS to the zone owner.
7. **“Bootstrap token” is one concept.** Legacy provider bootstrap data is not a Cloud Bootstrap Invite. Only add the latter if [the combined beta contract](https://github.com/getployz/ployz/issues/511) requires noninteractive acceptance.
8. **Legacy provisioning cleanup is unowned.** [Dashboard #103](https://github.com/getployz/ployz-dashboard/issues/103) already owns AWS/Hetzner/SSH removal; do not duplicate it.

## De-duplicated implementation issue packet

### Core repository

#### 1. [Generalize PushedToSeed into the beta multi-platform deploy receipt](https://github.com/getployz/ployz/issues/521)

Implement packet 1 of the [Build Adapter resolution](https://github.com/getployz/ployz/issues/513#issuecomment-4976478211): platform-independent index identity, per-platform seed/manifest/image data, v9 Namespace Revision Entry Identity, platform-aware ensure/pull, typed platform mismatch, SDK exports, fixtures, and mixed-architecture deploy acceptance.

**Overlap:** Rewrite/split open [core #370](https://github.com/getployz/ployz/issues/370), whose omnibus scope and inferred-platform/one-index details are superseded. Do not create a competing issue.

#### 2. Implement bounded `build.submit` with Dockerfile and Railpack adapters

After the receipt packet and [Railpack contract research #519](https://github.com/getployz/ployz/issues/519), implement packet 2: exact-SHA authenticated fetch, redaction and `.git` removal, fresh placement testimony, pinned ephemeral BuildKit, declared native-platform fan-out, all-or-nothing receipt, paged evidence, cancellation, timeout, and typed failures.

**Overlap:** This is the additive half of open #370, not another omnibus.

### Dashboard repository

#### 3. [Ingest and deduplicate GitHub push and check-suite events](https://github.com/getployz/ployz-dashboard/issues/109)

Verify and persist `push` and `check_suite` deliveries idempotently; resolve installation/repository identity and exact branch-head SHA; calculate changed paths; select `autoDeploy` services; and fan in once per environment. Duplicate and out-of-order deliveries must not create duplicate workflow attempts.

#### 4. [Add Build Records and the exact-commit build coordinator](https://github.com/getployz/ployz-dashboard/issues/110)

Persist frozen source/config fingerprints, core operation id, platform evidence, terminal receipt, reuse decision, and deployment linkage. Mint short-lived repo-scoped credentials, wait durably for CI when configured, build affected services, reuse unchanged receipts, and release exactly one environment deploy only after all builds succeed. Any build failure leaves serving truth unchanged.

#### 5. [Persist paged operation evidence and deliberate retry](https://github.com/getployz/ployz-dashboard/issues/108)

Replace status-only polling with a resumable `ops.watch` reader shared by build and deploy. Persist each event once, render typed timelines/failures, handle cancellation, and make Retry create a new attempt from the same frozen input without erasing prior evidence. This implements the still-unlanded decisions in closed Dashboard [#72](https://github.com/getployz/ployz-dashboard/issues/72) and [#71](https://github.com/getployz/ployz-dashboard/issues/71).

#### 6. [Project build receipts into the full namespace changeset](https://github.com/getployz/ployz-dashboard/issues/112)

Extend the existing environment snapshot with exact SHA, adapter config, Build Record ids, receipt identities, and requested platforms. Preserve the current whole-namespace compiler and reserve/submit seam. One push touching multiple services produces one frozen changeset and one deploy; route-only changes do not rebuild images.

#### 7. [Freeze canonical namespace and GitHub repository identities](https://github.com/getployz/ployz-dashboard/issues/113)

Migrate namespaces to frozen `<project-slug>-<environment-slug>` values unique org-wide, and use them consistently in deploy, runtime joins, Build Records, routes, and URLs. Store stable GitHub installation/repository ids alongside display names. Implements closed Dashboard [#69](https://github.com/getployz/ployz-dashboard/issues/69); no open implementation issue owns it.

#### 8. [Seal managed-hostname HTTPS and route operation evidence for beta](https://github.com/getployz/ployz-dashboard/issues/111)

Prove the existing automatic-hostname route through the sealed cross-repo fixture, display route/certificate progress from operation/runtime evidence, preserve the old serving route on failed replacement, and use exact collision errors. Treat custom-domain ownership/DNS lifecycle as a separate packet only if [#511](https://github.com/getployz/ployz/issues/511) makes it beta acceptance. Closed Dashboard [#73](https://github.com/getployz/ployz-dashboard/issues/73) contains stale mechanics and is not an implementation owner.

Do not create another acceptance-fixture issue: open core [#517](https://github.com/getployz/ployz/issues/517) owns the sealed hosted scenario. Do not create onboarding cleanup: open Dashboard [#103](https://github.com/getployz/ployz-dashboard/issues/103) owns legacy provisioning deletion. Add Cloud Bootstrap Invite/token UX only if #511 requires noninteractive onboarding.

## Dependency order

1. Core multi-platform receipt and Railpack contract research proceed independently.
2. Core `build.submit` depends on both.
3. Dashboard GitHub ingestion, Build Record schema, operation evidence, and namespace/repository identity can proceed against the frozen #513 contract.
4. The exact-commit coordinator and receipt-to-changeset handoff depend on the core build surface and Dashboard record/evidence seams.
5. Managed-hostname acceptance joins the completed deploy path.
6. Existing issue #517 seals onboarding → push → multi-platform build → one deploy → public HTTPS → visible failure/retry.

## Explicit exclusions

Hosted builders, registry export, build-cache affinity ([#520](https://github.com/getployz/ployz/issues/520)), builder labels/resource isolation, preview environments, templates, password/private route protection, rich telemetry retention, and SDK-owned passive projection are post-beta. They should not be pulled into these packets merely because adjacent legacy Cloud decisions called them “v1.”
