---
title: Dashboard Deploy Execution Skeleton
status: active
created: 2026-05-09
scope: standard
origin: user request to run the next dashboard slice for deployment execution
---

# Dashboard Deploy Execution Skeleton

## Problem Frame

`ployz-dashboard` already has early deployment machinery, including environment deployments, service snapshots, and Inngest workers. The next slice should make deployment execution an explicit dashboard workflow skeleton: create or reuse a deployment request, render a core deploy manifest placeholder, preview through a replaceable adapter seam, persist phase/status evidence, and apply through Inngest. This sets up later branch, portal, fresh-start, phased rollout, and recurring rollout work without putting core orchestration semantics into the dashboard.

## Scope Boundaries

In scope:
- Extend dashboard deployment persistence only where current `environment_deployment` records cannot track execution lifecycle, rendered manifest, preview result, adapter metadata, failure code, and phase snapshots.
- Add or reuse a server-side deploy execution service that renders a versioned manifest envelope from deployment snapshots and delegates preview/apply to a replaceable core adapter.
- Add or extend an Inngest deployment worker that advances deployment status through render, preview, and apply steps with idempotent state updates.
- Add events and enqueue helpers for deployment requests if the current event surface is incomplete.
- Cover schema/model/service/worker behavior with focused tests.

Out of scope:
- Final branch, portal, fresh-start, snapshot clone, or volume move UX.
- Calling a real remote `ployz` daemon or CLI.
- Full phase-aware UI.
- Core deploy phasing implementation; dashboard should accept and persist core-shaped phase evidence but not define the semantics.

## Existing Patterns

- Inngest client/events/functions live in `src/inggest/client.ts`, `src/inggest/events.ts`, and `src/inggest/functions/index.ts`.
- Current deploy worker code lives under `src/inggest/functions/environment-deployments/`.
- Deployment persistence and service snapshot behavior live in `src/models/services/environment-deployments.server.ts`, `src/models/services/environment-deployments.ts`, `src/models/services/service-deployments.server.ts`, and `src/models/services/service-deployments.ts`.
- Deployment tables live in `src/db/schema.ts`.
- Existing deployment-related tests live near `src/models/services/*deployment*.test.ts` and `src/inggest/functions/environment-deployments/*.test.ts`.

## Key Decisions

1. Keep dashboard as workflow owner, not orchestration authority.
   Dashboard may decide when to request preview/apply/resume, but core remains responsible for validating deploy semantics and returning phase evidence.

2. Store manifest and preview as versioned JSON envelopes.
   The dashboard project is early, and the core contract will move. A versioned envelope keeps the DB durable without pretending the contract is final.

3. Prefer evolving the existing environment deployment model over creating a parallel deploy system.
   The updated `main` already has deployment-specific files, so this slice should strengthen that path instead of duplicating it.

4. Make the core adapter injectable.
   Tests should not shell out, and the first implementation can be a local placeholder adapter. Later slices can swap in CLI, daemon RPC, or hosted API transport.

5. Treat Inngest as the timing/retry shell.
   Inngest should make progress through explicit dashboard state transitions and adapter calls. It should not infer safety or mutate cluster state outside a core deploy command.

## Implementation Units

### U1: Reconcile current deployment model with execution requirements

Files:
- Modify `src/db/schema.ts`
- Create generated migration under `drizzle/` if schema changes are needed
- Modify `src/models/services/environment-deployments.ts`
- Modify `src/models/services/environment-deployments.server.ts`
- Modify `src/models/services/environment-deployments.test.ts`

Approach:
- Inspect the current `main` deployment schema before adding fields; reuse existing deployment status, workflow, and manifest columns where they exist.
- Ensure an environment deployment can represent pending/rendering/previewing/ready/applying/succeeded/failed/cancelled lifecycle or map current statuses cleanly to that lifecycle.
- Ensure phase evidence can be persisted either through an existing field/table or a small new phase snapshot table.
- Keep existing service snapshot and deploy diff behavior working.

Test scenarios:
- Creating a deployment request still writes deployment and snapshot records.
- In-progress execution statuses remain valid in model schemas.
- Phase or preview evidence can be persisted and read back with typed model helpers.

### U2: Add deploy manifest renderer and core adapter seam

Files:
- Create or modify `src/models/services/service-deploy-env.server.ts`
- Create or modify `src/models/services/environment-deployments.server.ts`
- Create or modify focused tests near `src/models/services/service-deploy-env.server.test.ts` and `src/models/services/environment-deployments.test.ts`

Approach:
- Render a versioned manifest envelope from an environment deployment and its service deployment snapshots. Include environment id, deployment id, services, source snapshot configs, and a `version`.
- Define a `DeployCoreAdapter` interface with `previewDeployment(manifest)` and `applyDeployment(manifest)`.
- Provide a deterministic local placeholder adapter that returns one phase and a successful apply result. Name it clearly as a placeholder.
- Add dependency injection helpers for tests.

Test scenarios:
- Rendering produces a deterministic manifest envelope from snapshots.
- Rendering an empty deployment produces an empty services list rather than failing.
- Placeholder preview returns one phase whose id is stable for the deployment.
- Placeholder apply returns a success result tied to the same deployment id.

### U3: Add deployment execution workflow service

Files:
- Modify `src/models/services/environment-deployments.server.ts`
- Modify or create focused tests near `src/models/services/environment-deployments.test.ts`

Approach:
- Add server-only functions to load a deployment with snapshots, persist rendered manifest and preview, upsert phase evidence, mark status transitions, and record failures.
- Keep each state transition explicit and idempotent where possible: if a deployment is already succeeded, applying again should return the stored terminal state instead of duplicating work.
- Avoid auth/session coupling inside the worker path; it should operate by deployment id after the original user-triggered request created the row.

Test scenarios:
- Starting execution persists a manifest.
- Preview persistence stores phase evidence and marks the deployment ready/applicable.
- Apply success sets terminal success status and finished timestamp.
- Adapter failure marks failed with a structured failure code/message.
- Re-running a terminal deployment is a no-op.

### U4: Wire Inngest deployment events and worker

Files:
- Modify `src/inggest/events.ts`
- Modify `src/inggest/functions/environment-deployments/deploy.ts`
- Modify `src/inggest/functions/environment-deployments/deploy.test.ts`
- Modify `src/inggest/functions/index.ts`
- Modify deployment server functions that create deployment requests

Approach:
- Reuse existing deployment events and worker if present; otherwise add `deployment/environment.requested` helpers.
- After a user creates a deployment request, enqueue the deployment event once.
- Ensure the worker calls the execution service in clear steps: load/render, preview/persist, apply/persist.
- Add failure handling that marks a non-terminal deployment as failed when the worker crashes outside handled adapter failures.

Test scenarios:
- Snapshot/request creation enqueues exactly one deployment event with deployment id and environment context.
- Worker happy path runs steps in order and returns final status.
- Worker skips cleanly when the deployment id is invalid or missing.
- Worker failure handler marks a non-terminal deployment as failed.

### U5: Verification and traceability

Files:
- Modify query/collection files only if type fallout requires it.
- Keep UI changes out unless needed to compile.

Approach:
- Run focused deployment tests first, then project typecheck and test suite.
- Avoid docs churn beyond this plan.

Test scenarios:
- `pnpm test -- environment-deployments service-deploy-env deploy`
- `pnpm typecheck`
- `pnpm test`

## Dependencies and Sequencing

U1 must establish or confirm the persistence surface before U2-U4. U2 provides the adapter/manifest seam. U3 turns the seam into durable deployment state transitions. U4 exposes it through Inngest. U5 verifies the full slice.

## Assumptions

- The first adapter is intentionally local and deterministic; real core transport is a later slice.
- The dashboard can enqueue deployment execution immediately after a deployment request is created.
- Existing `main` deployment files should be treated as source of truth over the earlier pre-fast-forward snapshot model.
