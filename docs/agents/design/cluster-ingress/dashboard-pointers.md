# Dashboard exploration for cluster ingress and private access

Paths are repo-relative. `D` = `dashboard/src`. `O` = `D/routes/_protected/cloud/$organizationSlug`. Inspect-only run; no files changed.

## 1. Onboarding today

**Sign-up to first deploy.**
- Every new user gets a personal Organization. `D/server/auth.server.ts:400` (user `create.after`) calls `handleUserCreated` at `D/modules/organization/organization-state.server.ts:244`. No Server or Cluster exists at that point.
- GitHub sign-in lands on `/cloud` (`D/routes/_public/auth.tsx:52`). `D/routes/_protected/cloud/index.tsx:15-29` redirects to `/cloud/$org/~`, or to `/cloud/$org/new` when `?welcome=true` (only the dev sign-up in `D/routes/_public/-components/LoginPanel.tsx:52` sets it).
- The Projects page redirects an Organization with no projects straight to New project: `O/_org/~/index.tsx:25`.
- New project is `O/_project/new.tsx:17-43`, a dotted canvas holding `ServiceCreateCommand` (`D/components/service-create-command.tsx`, which creates the project and Environment, then navigates at :268-345).

**Staging before any Server: yes.** A user can create a Project and stage Services with zero Servers. The canvas bottom bar swaps Deploy for a pink `intent` **Add servers** button when `noServers` (`O/_project/$projectSlug/$environmentSlug/-components/canvas/BottomBar.tsx:133,228-229`, using `AddServersDialog`). `docs/user/getting-started/quick-start.md:49-72` documents this order.

**No per-Organization or per-Cluster setup wizard.** There is no "onboarding" module (grep finds nothing). The first-Server experience is the Servers page:
- Route `O/_org/~/servers/index.tsx`. Page state is a union in `servers/-components/servers-page-state.ts:7-28` (`loading | unreachable | first-run | fleet`). `foundingFirstServer` is at :34.
- `servers/-components/first-run-card.tsx:22` renders the "Add your first server" card (three steps at :12: run the line, review, enroll), minting a Fleet Token in place.
- `servers/-components/add-servers-dialog.tsx:24` is the Fleet Token mint plus arrivals flow, also used from the bottom bar.
- Waiting and enrolling. `servers/-components/waiting-list.tsx:47` (`IntakeTray`), `waiting-detail-sheet.tsx`, `enroll-sheet.tsx:19`, `enrolling-list.tsx`, `use-waiting.ts`.
- Server side lives in `D/modules/machines/waiting/`: `checkin.server.ts`, `fleet-token.server.ts`, `enroll.server.ts` (plan), `enrollment-run.server.ts:26` (`openFounderAttempt`), and `server-enrollment.inngest.ts`.

**Defaults set at founding.**
- Roles. `EnrollPolicy` is in `D/modules/machines/waiting/server-enrollment.ts:31-42`. The Enroll sheet defaults `runs = {services, ingress, builds}` all true (`enroll-sheet.tsx:36`). Its "Web traffic" checkbox is at :106. Auto-enroll on arrival hardcodes the same at `enroll.server.ts:442`. Policy is built at `enroll.server.ts:133-138`.
- Founder pick. `enroll.server.ts:114-127`, with the role assigned at :161.
- Cluster Domain is **not** reserved at founding. It is reserved lazily on the first write or Deploy that needs a generated hostname. See `D/modules/config-store/domain-evidence.server.ts:155,165` calling `reserveClusterDomain` (`D/modules/cluster-domain/cluster-domain.server.ts:32`). This matches `dashboard/CONTEXT.md:136-137`.
- An Organization has exactly one Cluster (`CONTEXT.md:11`, "Organization Cluster"). So "per-Cluster" settings today means per-Organization settings.

## 2. Settings surfaces

- **Organization settings.** The route is `O/_org/~/settings.tsx`. Sections come from the `?section=` literal union at :30 (`general | members | tokens | github | builds`). The rail entries are at `D/components/dashboard-navigation-model.ts:188-191`. The General body at :71-77 renders `OrganizationNameSection`, `EnrollmentSection`, `ClusterDomainSettings` (:104-113) and `StoreOrganizationDanger`. A Cluster-level "Web traffic: public ports vs Cloudflare Tunnel" and "Private access: Tailscale" fit either as new sections in General beside `ClusterDomainSettings`, or as a new `section` literal (for example `network`) plus a rail entry.
- **Cluster Domain UI.** `O/_org/-components/ClusterDomainSection.tsx:61` (137 lines; status copy at :25, excluded Servers at :45). Its data comes from `getClusterDomainCollection` and `checkClusterDomainNowServerFn` (`D/modules/cluster-domain/cluster-domain.functions.ts`). Status logic is in `D/modules/cluster-domain/cluster-domain.ts` (`clusterDomainStatus`).
- **Per-Server web traffic.** The page is `O/_org/~/servers/$serverId.tsx`, with the hook at :136 and the query at :57. The UI is in `servers/-components/server-web-traffic-section.tsx:13` (`ServerWebTrafficSection`, switch "Takes web traffic") and `web-traffic-dialog.tsx:18` (the off confirm, `last | shared` reach copy). The module is `D/modules/machines/server-web-traffic{.ts,-view.ts,.functions.ts,.hooks.ts,.queries.ts,.server.ts,.inngest.ts}`.

## 3. Approvals, Inngest attempts, PloyzSession

**Asking for an Approval.**
- Types are in `D/modules/approvals/approvals.ts`. `OperationVerb` is at :11, and must stay on one line because Rust parses it. `OperationReview` is at :14 and `ApprovalReview` at :35-37.
- The gate is `gateOperation` at `D/modules/approvals/approvals.server.ts:542`. The dashboard skips it unless the Environment is Protected. It passes when there are no effects or no human approver, and otherwise digests the ask and calls `requestOperationApproval`. `OperationAsked` (`subject`, `verb`, `name`, `preview`, `effects`, `protected`, `apply`) is at :452.
- The operation path is `D/modules/operations/operations.server.ts`. `gate()` is at :55. `setServer` at :121-132 is the template: `planIngressOff` (`D/modules/machines/server-operations.server.ts:117,236`) builds an `OperationAsked` only when ingress goes off, and otherwise calls `unasked(...)`.
- The registry is `D/modules/operations/operations.ts`. `OPERATION_TOOLS` is at :16. `OPERATIONS` is at :96, and `server.set` at :190 shows `requires`, `gate: "approval"`, `idempotency` and `surfaces` (cli, agent, ui).
- Destructive Effects are `DestructiveKind` in `core/crates/ployz-store/src/review/approval.rs:54-79` (TS through `@ployz/sdk`). A Publish's effects come from the Store review (`core/crates/ployz-store/src/review/`). A "DNS effect in the Publish review" would be a new `DestructiveKind` or a separate review row there.

**Steps to add a new verb (for example `use_ingress`).**
1. Append it to the `OperationVerb` union (`approvals.ts:11`).
2. Add an `OperationVerb` variant in `core/crates/ployz-store/src/review/required.rs` (TS through `@ployz/sdk`), then its `Verb` variant and `noun()` in `core/crates/ployz/src/approval.rs` (`From<OperationVerb> for Verb`). The test `every_operation_verb_cloud_sends_is_a_verb` fails otherwise.
3. Add `OPERATION_TOOLS` and `OPERATIONS["…"]` entries (`operations.ts`), plus an `operations.server.ts` function that builds `OperationAsked` and calls `gate`.
4. If it destroys something, add a `DestructiveKind` (Rust) and `describe()` copy.
5. Check `approvalSubject` copy (`approvals.ts:63`) and the parity tests `D/modules/operations/operations.parity.test.ts` and `agent-acts.parity.test.ts`.

**Running a Machine operation as an Inngest attempt** (`D/modules/machines/server-web-traffic.inngest.ts`).
- The handler `applyServerWebTraffic` at :28-42 runs `normalize-request`, then `bind-run` (persist `runId`), then `execute-once`, then `sendEvent` cluster-domain sync.
- `attemptLifecycle` at :44-57 is required by `dashboard/AGENTS.md`. It owns the run, the cancel and the stale sweep. Concurrency is keyed on `organizationId` with limit 1.
- The server half is `server-web-traffic.server.ts`. `requestServerWebTraffic` is at :73, `bindWebTrafficRun` at :132 and `applyWant` at :177-195 (`session.updateMachine` then `startIngress` or `stopIngress`, with a timeout). `executeWebTrafficOnce` at :218 claims pending to running and calls the Engine once.
- The attempt table is `D/modules/machines/tables.ts:234`. The event is `D/modules/inngest/events.ts:96,358`. Registration is `D/modules/inngest/index.ts:58`.

**Adding a PloyzSession method.**
1. Rust impl on the session in `core/crates/ployz/src/sdk.rs` (pattern at :1088 `start_ingress`).
2. napi binding in `core/crates/ployz-sdk/src/lib.rs` (pattern at :731-745, `#[napi(ts_return_type=…)]`).
3. `pnpm sdk` (runs `core/scripts/build-cloud-sdk.sh`) regenerates `core/crates/ployz-sdk/generated/native.d.ts`. `index.d.ts` is tracked and re-exports it.
4. Add the member to `interface PloyzSession` (`D/modules/runtime/ployz.server.ts:86`, near :126-128) and wire it with `sdkPromise` (:371-372).
5. Update fakes that implement it (for example `D/modules/machines/server-web-traffic.server.postgres.test.ts`). Value imports of `@ployz/sdk` are allowlisted in `D/modules/runtime/ployz-sdk-boundary.test.ts:6`.

## 4. Managed hostname expansion

- Cloud supplies the Cluster Domain as evidence. `gatherDomainEvidence` (`D/modules/config-store/domain-evidence.server.ts:140-180`) puts `clusterDomain(row)` (:37) into `ConfigDomainEvidence.cluster_domain`.
- The Store expands it. `DomainEvidence.cluster_domain` is in `core/crates/ployz-store/src/domain/mod.rs:43`. `expand()` at :998-1022 turns each `managed_hostnames` `prefix` into a route `"{prefix}.{cluster_domain}"`. It is called from `core/crates/ployz-store/src/deployment/lowering.rs:623`. Prefix uniqueness and the 63-char label checks are at `domain/mod.rs:702-715,1032-1045`.
- Generated prefixes are already one label, so `svc-env.<zone>` falls out unchanged.
- **Tunnel Zone plug-in point.** Either make `cluster_domain` evidence carry the Tunnel Zone when the Cluster is in tunnel mode (Cloud-only change in `domain-evidence.server.ts`), or add a sibling `tunnel_zone` field to `DomainEvidence` that `expand()` prefers. The second keeps the two terms distinct, as decision 2 asks.
- Also gated on the Cluster Domain, and in need of a tunnel branch:
  - `reserveClusterDomain` calls, which would reserve a Hosted DNS name you don't need.
  - The apex sync in `D/modules/cluster-domain/sync.server.ts:140`.
  - `ingress-probe.server.ts`, the port 80 probe.
  - Status reasons `port_80` and `ingress_down` (`domain-evidence.server.ts:42-47`).

## 5. Checks and verify-dashboard

Run from `dashboard/`. CI is the gate; locally run only focused checks (root and dashboard `AGENTS.md`).
- `pnpm typecheck` (tsc), `pnpm lint` (oxlint type-aware).
- Unit: `pnpm exec vitest run --project unit <files>` (`src/**/*.test.ts(x)`).
- Postgres: `pnpm exec vitest run --project postgres <files>` (`*.postgres.test.ts`). It needs Docker, since the global setup starts `ployz-test-*` containers (`D/test/postgres.global-setup.ts`).
- Static analysis: `--project static-analysis` (`*.static.test.ts`).
- `pnpm sdk` after Rust SDK changes. `bash scripts/pr-check.sh <stage>` runs one CI stage.
- Rust side of a verb: `cargo nextest run -p ployz approval` in `core/`.

verify-dashboard (`.claude/skills/verify-dashboard/SKILL.md`) in 5 lines:
1. `scripts/verify/up.sh` builds `node_modules` and the SDK if stale, starts a per-checkout Postgres and `vite dev`, and seeds org `ada` / project `shop`. You are signed in as Ada. `--fresh` restarts.
2. `SERVERS=1|2 scripts/verify/up.sh` adds Inngest, the worker and real Boat Machines enrolled via `../core/scripts/verify-cluster`. `DAEMON=checkout` uses this branch's daemon. Use it for any runtime UI, web traffic toggles included.
3. `FAKE_SERVERS=1` fakes the Runtime Watch with four Servers for list-only UI. Hosted DNS is always fake (`<slug>.ployz.test`, throwaway CA).
4. Drive with `agent-browser --session $(cat .verify/run/session)`, pasting the two lines from `.verify/run/info`. Use `snapshot -i -c`, click refs, and save screenshots to absolute `.verify/evidence/*.png`. Reload after writes. `scripts/verify/as.sh grace|linus` switches role.
5. `scripts/verify/doctor.sh` health-checks the instance. `scripts/verify/down.sh` cleans up, and the instance also idles out after 15 min.

## Summary

1. New users get a personal Organization (one Cluster each). Empty orgs redirect to `/new`, where a Project and Services stage with no Server, and the bottom bar swaps Deploy for **Add servers**. There is no setup wizard; first-Server UI is `first-run-card.tsx` plus the waiting/enroll tray.
2. Founding sets services, ingress and builds all on (`enroll-sheet.tsx:36`, `enroll.server.ts:442`). The Cluster Domain is reserved lazily on the first generated hostname, not at founding.
3. Cluster-level toggles belong in `O/_org/~/settings.tsx` (General beside `ClusterDomainSettings`, or a new `section` literal). Per-Server web traffic is `server-web-traffic-section.tsx` and `web-traffic-dialog.tsx`.
4. A new verb touches `approvals.ts:11`, Rust `Verb` (`approval.rs:103`), `operations.ts` and `operations.server.ts`. Machine ops follow `server-web-traffic.inngest.ts` with `attemptLifecycle`. Session methods go Rust `sdk.rs`, then napi `lib.rs`, then `pnpm sdk`, then `ployz.server.ts:86`.
5. Expansion is `ployz-store/src/domain/mod.rs:998` fed by `domain-evidence.server.ts`, which is the Tunnel Zone hook. Verify with `pnpm typecheck`, `pnpm lint`, focused `vitest --project unit|postgres`, and `SERVERS=1 scripts/verify/up.sh` for runtime UI.
