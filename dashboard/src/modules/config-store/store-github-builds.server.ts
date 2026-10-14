import "@tanstack/react-start/server-only";
import type { GithubBuild, GithubClaims } from "@ployz/sdk";
import { Effect } from "effect";
import { cancelDispatchedGithubRun, cancelStoreGithubBuilds, connectionsOf } from "#/modules/config-store/config-store.server";
import { cloudStore, refusedWith, storeTry } from "#/modules/config-store/store-sdk.server";
import type { StoreRefused } from "#/modules/config-store/store.contract";
import { pinStoreSources } from "#/modules/config-store/store-deployment.server";
import { organizationAccess } from "#/modules/github/github-access";
import { cancelGithubRun, checkGithubBuildWorkflow, dispatchGithubBuildWorkflow, githubRunCompleted } from "#/modules/github/github-build.server";
import { verifyGithubOidcToken } from "#/modules/github/github-oidc.server";
import { installationRefusal } from "#/modules/github/github-token.server";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createGithubBuildRunCompletedEvent, type ConfigDeploymentAdmittedEventData } from "#/modules/inngest/events";
import { requirements, serversLacking } from "#/modules/runtime/daemon-compat";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";
import { AppConfig } from "#/server/config.server";
import { BuildGrantUnavailable, Conflict, Forbidden, NotFound, Unauthorized, Validation } from "#/server/public-error";

/**
 * GitHub as a Builder for Store Deployments. Before the runner claims a Deployment, each Git Service whose walk
 * (Preferred Builder, then the Build Order, both from the Store) starts with GitHub goes to GitHub Actions: Cloud
 * checks the repository's workflow, dispatches it, and waits while the runner checks in with its OIDC token for its
 * Build Grant and build inputs, reports its Build Steps, and pushes. The Store records every step; Rust touches the
 * secrets and the Machines. The runner then delivers what GitHub built, and builds on the servers whatever GitHub
 * skipped when the walk has them next.
 *
 *   plan ─▶ start (reuse | skip | dispatch) ─▶ wait ⇄ check ─▶ built | failed | skipped ─▶ run-deployment
 */

/** How long a run may build after it checked in before Cloud cancels it; the daemon's grant lives 3h. */
export const GITHUB_RUN_BUDGET_MS = 2 * 60 * 60_000;
/** How often Cloud looks at a dispatched run between Workflow run webhooks. */
export const GITHUB_CHECK_INTERVAL = "10m";
/** How long GitHub has to start a build a later Builder could take. */
export const START_WITHIN_MINUTES = 3;

/** One Git build the walk hands to GitHub first. No secret: it is a step's output. */
export type StoreGithubTarget = {
  build: string;
  service: string;
  repository: string;
  repositoryId: number;
  installationId: number | null;
  /** Another Builder follows in its walk, so GitHub gets START_WITHIN_MINUTES to start it. */
  hasNext: boolean;
};

/**
 * Whether any Server takes builds now, as the Cluster reports it. No answer reads as yes: the runner then says why the
 * Servers couldn't build.
 */
const serversBuild = (organizationId: string) => firstRuntimeFrame(organizationId).pipe(
  Effect.map((frame) => frame === null || frame.machines.some(({ machine }) => machine.accepts_builds)),
  Effect.orElseSucceed(() => true),
);

/**
 * The Servers whose records say they can't read a Build Grant back. Any Server may mint the grant, so one such Server
 * is enough to send the build to its next Builder. No answer names none: the build is attempted.
 */
const serversLackingGrants = (organizationId: string) => firstRuntimeFrame(organizationId).pipe(
  Effect.map((frame) => frame === null ? [] : serversLacking(
    frame.machines.map(({ machine }) => ({ name: machine.name, capabilities: machine.runtime.capabilities ?? [] })),
    requirements.buildGrantInspect,
  )),
  Effect.orElseSucceed((): string[] => []),
);

/**
 * Pin the Deployment's Git Services and list those whose walk starts with GitHub and haven't started; a walk that
 * starts with the Servers starts with its next Builder when no Server takes builds. Anything else (an unreadable
 * source, a Deployment no longer wanted) is left to the runner, which records it.
 */
export const planStoreGithubBuilds = Effect.fn("StoreGithub.plan")(function* (data: ConfigDeploymentAdmittedEventData) {
  const store = yield* cloudStore;
  const sources = yield* pinStoreSources(store, data.organizationId, data.deploymentId).pipe(
    Effect.catchTag("SourceUnreadable", () => Effect.succeed([])),
    Effect.catchIf(refusedWith("conflict"), () => Effect.succeed([])),
  );
  // A build GitHub skipped (with why) belongs to the next Builder.
  const pending = sources.filter((source) => source.status === "pending" && source.message === null);
  const servers = pending.some((source) => source.builders[0] === "servers" && source.builders.includes("github"))
    ? yield* serversBuild(data.organizationId)
    : true;
  return pending.flatMap((source): StoreGithubTarget[] => {
    const walk = servers ? source.builders : source.builders.filter((builder) => builder !== "servers");
    if (walk[0] !== "github") return [];
    return [{
      build: `${data.deploymentId}.${source.service}`,
      service: source.service,
      repository: source.repository,
      repositoryId: source.repository_id,
      installationId: source.access.type === "github-installation" ? source.access.installationId : null,
      hasNext: walk.length > 1,
    }];
  });
});

export type StoreGithubStart = { kind: "dispatched"; runId: number } | { kind: "done" };
const done = { kind: "done" } as const;

/**
 * Start a build on GitHub: skip GitHub at once when it can't take it (no GitHub App access, no build workflow, several
 * platforms, GitHub or the Cluster unreachable), reuse the Service's latest image when a Machine still holds it for
 * this commit, else dispatch the workflow on the runner native to its platform. Workflow inputs are visible on GitHub,
 * so they never carry secrets: the runner fetches those at check-in.
 */
export const startStoreGithubBuild = Effect.fn("StoreGithub.start")(function* (organizationId: string, target: StoreGithubTarget) {
  const store = yield* cloudStore;
  const skip = (message: string) => storeTry(() => store.githubSkip(target.build, message)).pipe(
    // It started or ended meanwhile (a retried step): nothing to do.
    Effect.catchIf(refusedWith("conflict"), () => Effect.void),
    Effect.as<StoreGithubStart>(done),
  );
  if (target.installationId === null) return yield* skip(`GitHub builds need the Ployz GitHub App on ${target.repository}`);
  const installationId = target.installationId;
  const access = organizationAccess(organizationId, installationId);
  return yield* Effect.gen(function* () {
    const workflow = yield* checkGithubBuildWorkflow(access, target.repositoryId);
    const repository = workflow.fullName ?? target.repository;
    if (workflow.readiness === "no_permission") return yield* skip(`the Ployz GitHub App can't run Actions in ${repository}`);
    if (workflow.readiness !== "ready" || !workflow.fullName || !workflow.defaultBranch) {
      return yield* skip(`${repository} has no .github/workflows/ployz-build.yml on its default branch`);
    }
    const lacking = yield* serversLackingGrants(organizationId);
    if (lacking.length > 0) return yield* skip(`GitHub builds need a newer Ployz on ${lacking.join(", ")}`);
    const connections = yield* connectionsOf(organizationId);
    const start = yield* storeTry(() => store.githubStart(target.build, connections));
    if (start.kind !== "dispatch") return done;
    const config = yield* AppConfig;
    const fullName = workflow.fullName;
    const run = yield* dispatchGithubBuildWorkflow({
      access, fullName, defaultBranch: workflow.defaultBranch,
      inputs: { build: target.build, cloud: config.app.url.origin, runner: start.runner },
    });
    // The run must not build untracked: whatever stops Cloud recording it (cancelled or replaced meanwhile, a
    // failure it retries) cancels it first.
    const cancel = cancelGithubRun({ access, fullName, runId: run.runId }).pipe(Effect.ignore);
    const handed = yield* storeTry(() => store.githubDispatched(target.build, {
      run_id: run.runId, run_url: run.runUrl, workflow_ref: run.workflowRef, repository: fullName, installation_id: installationId,
    })).pipe(
      Effect.as(true),
      Effect.catchIf(refusedWith("conflict"), () => cancel.pipe(Effect.as(false))),
      Effect.onError(() => cancel),
    );
    if (!handed) return done;
    return { kind: "dispatched", runId: run.runId } satisfies StoreGithubStart;
  }).pipe(
    // GitHub failing before dispatch is GitHub being unusable, not the build failing.
    Effect.catchTag("GithubObservationError", (error) => skip(`GitHub couldn't take the build: ${error.message}`)),
    Effect.catchTag("GithubDisconnected", (error) => skip(error.message)),
    // Started or ended by a retried step already.
    Effect.catchIf(refusedWith("conflict"), () => Effect.succeed(done)),
  );
});

/** Whether GitHub says the run completed; GitHub unreachable reads as still running. */
const runEnded = (build: GithubBuild) => githubRunCompleted({
  access: organizationAccess(build.organization, build.run.installation_id), fullName: build.run.repository, runId: build.run.run_id,
}).pipe(Effect.orElseSucceed(() => false));

export type StoreGithubCheck = "waiting" | "done" | { failed: string };

/**
 * One look at a dispatched build, after its run completed (the webhook), its final report came, or a wait timed out.
 * A Deployment cancelled or replaced meanwhile stops it. An Organization that can't use the installation any more
 * (disconnected, or GitHub suspended or uninstalled it) can't read what GitHub built, so its builds stop and the
 * Deployment fails with why. A run past its budget
 * is cancelled and ended. A run that hasn't checked in by the start limit (`startLimit`: another Builder follows) is
 * skipped and cancelled; GitHub keeps one that started.
 */
export const checkStoreGithubBuild = Effect.fn("StoreGithub.check")(function* (
  organizationId: string, target: StoreGithubTarget, seen: { ended: boolean; startLimit: boolean },
) {
  const store = yield* cloudStore;
  const build = yield* storeTry(() => store.githubBuild(target.build));
  if (build.status !== "building") return "done" as const;
  const deploymentId = build.id.slice(0, build.id.indexOf("."));
  const view = yield* storeTry(() => store.read(organizationId, { query: "deployment", id: deploymentId }));
  if (view.status !== "queued" && view.status !== "running") {
    yield* cancelStoreGithubBuilds(deploymentId);
    return "done" as const;
  }
  const refusal = yield* installationRefusal(organizationId, build.run.installation_id);
  if (refusal !== null) {
    yield* cancelStoreGithubBuilds(deploymentId);
    return { failed: refusal.message };
  }
  const finish = (timedOut: boolean) => Effect.gen(function* () {
    const connections = yield* connectionsOf(organizationId);
    const finished = yield* storeTry(() => store.githubFinish(target.build, timedOut, connections));
    return finished === "waiting" ? "waiting" as const : "done" as const;
  });
  if (build.checked_in_at !== null && Date.now() - build.checked_in_at * 1000 > GITHUB_RUN_BUDGET_MS) {
    yield* cancelDispatchedGithubRun(build.run);
    return yield* finish(true);
  }
  if (build.grant === null) {
    if (seen.ended || (yield* runEnded(build))) return yield* finish(false);
    if (!seen.startLimit) return "waiting" as const;
    const found = yield* finish(true);
    if (found === "done") yield* cancelDispatchedGithubRun(build.run);
    return found;
  }
  if (seen.ended || build.ended !== null || (yield* runEnded(build))) return yield* finish(false);
  return "waiting" as const;
});

/** Whether a build GitHub holds is still on GitHub: a final report may settle it before a wait begins. */
export const storeGithubBuildOpen = Effect.fn("StoreGithub.open")(function* (target: StoreGithubTarget) {
  const store = yield* cloudStore;
  return (yield* storeTry(() => store.githubBuild(target.build))).status === "building";
});

const bearer = (request: Request) => /^Bearer (\S+)$/.exec(request.headers.get("authorization") ?? "")?.[1] ?? null;

/** The runner's verified OIDC claims; the Store checks them against the build's repository, workflow and run. */
const runnerClaims = Effect.fn("StoreGithub.claims")(function* (request: Request) {
  const config = yield* AppConfig;
  const token = bearer(request);
  if (!token) return yield* new Unauthorized();
  const claims = yield* verifyGithubOidcToken(token, config.app.url.origin).pipe(Effect.mapError(() => new Unauthorized()));
  return { repository_id: claims.repository_id, job_workflow_ref: claims.job_workflow_ref, run_id: claims.run_id, event_name: claims.event_name } satisfies GithubClaims;
});

/** The Store's refusal, as the runner's API answers it. */
const refused = (error: StoreRefused) => {
  const { message } = error;
  switch (error.code) {
    case "unauthenticated": return new Forbidden({ message });
    case "not_found": return new NotFound({ message });
    case "conflict": return new Conflict({ message });
    case "invalid_argument": return new Validation({ message });
    default: return new BuildGrantUnavailable({ cause: error });
  }
};

/**
 * A runner's one check-in for a Store build: its Build Grant, pinned commit, expected fingerprint, the ployz version
 * that computed it, and the one-Service deployment whose variables carry the build secrets.
 */
export const checkInStoreGithubBuild = Effect.fn("StoreGithub.checkIn")(function* (request: Request, buildId: string) {
  const claims = yield* runnerClaims(request);
  const store = yield* cloudStore;
  return yield* Effect.gen(function* () {
    const build = yield* storeTry(() => store.githubBuild(buildId));
    const refusal = yield* installationRefusal(build.organization, build.run.installation_id);
    if (refusal !== null) return yield* new Forbidden({ message: refusal.message });
    const connections = yield* connectionsOf(build.organization);
    return yield* storeTry(() => store.githubCheckIn(buildId, claims, connections));
  }).pipe(Effect.catchTag("StoreRefused", (error) => Effect.fail(refused(error))));
});

const MAX_STEPS_REPORT_BYTES = 16 * 1024 * 1024;

/**
 * A runner's batch of Build Steps for a Store build, into its build log. The final one (with `platforms`) ends the
 * build at once, its grant ended and its receipt written, and wakes the walk.
 */
export const recordStoreGithubBuildSteps = Effect.fn("StoreGithub.steps")(function* (request: Request, buildId: string, text: string) {
  const claims = yield* runnerClaims(request);
  if (text.length > MAX_STEPS_REPORT_BYTES) return yield* new Validation({ message: "The report is too large." });
  const report: unknown = yield* Effect.try({ try: () => JSON.parse(text), catch: () => new Validation({ message: "The report is not Build Steps." }) });
  const store = yield* cloudStore;
  return yield* Effect.gen(function* () {
    const build = yield* storeTry(() => store.githubBuild(buildId));
    const refusal = yield* installationRefusal(build.organization, build.run.installation_id);
    if (refusal !== null) return yield* new Forbidden({ message: refusal.message });
    const reported = yield* storeTry(() => store.githubReport(buildId, claims, report));
    if (reported.ended) {
      const connections = yield* connectionsOf(build.organization);
      // The walk's next look ends it otherwise.
      yield* storeTry(() => store.githubFinish(buildId, false, connections)).pipe(
        Effect.catch((error) => Effect.logWarning("Could not end a reported GitHub build.", error)),
      );
      yield* sendInngestEvent(createGithubBuildRunCompletedEvent({ id: `reported-${buildId}`, runId: build.run.run_id })).pipe(
        Effect.catch((error) => Effect.logWarning("Could not wake the walk for a reported GitHub build.", error)),
      );
    }
    return { received: reported.received };
  }).pipe(Effect.catchTag("StoreRefused", (error) => Effect.fail(refused(error))));
});
