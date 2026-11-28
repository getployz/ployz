import "@tanstack/react-start/server-only";
import type { ConfigStore, DeploymentStep } from "@ployz/sdk";
import { Effect, Option } from "effect";
import { cloudStore, deployDialOf, refusedWith, storeTry } from "#/modules/config-store/store-sdk.server";
import { callStore, readStore, requestChecks } from "#/modules/config-store/config-store.server";
import { cancelStoreGithubBuilds, newerAttempt } from "#/modules/config-store/store-github-builds.server";
import { fromGithub, pinStoreSources, sourceIdentity, SourceUnreadable } from "#/modules/config-store/store-sources.server";
import { deploymentRun } from "#/modules/config-store/tables";
import { discardFailedRestore, type FailedRestore, releasePlacementClaims } from "#/modules/volume-placement/volume-placement.deploy.server";
import { extractUpload, releaseUploads, uploadOwner } from "#/modules/config-store/upload.server";
import { eq, lt } from "drizzle-orm";
import { sourcePaths } from "#/modules/github/github-source.server";
import { GithubSources } from "#/modules/github/github-sources.server";
import { GithubAuthority } from "#/modules/github/github-authority.server";
import type { ConfigDeploymentAdmittedEventData } from "#/modules/config-store/config-store.events";
import { Database } from "#/server/database.server";
import type { InngestClient, InngestRunStatus } from "#/modules/inngest/client";
import type { Polar } from "#/modules/billing/polar-provider.server";
import type { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { GithubApi } from "#/modules/github/github-observation.api";
import type { AppConfig } from "#/server/config.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";
import type { CloudStore } from "#/modules/config-store/store-sdk.server";
import { admitDeploy } from "./admit-deploy";

/** What Cloud's Store workers need from Cloud. */
export type StoreServices = AppConfig | Database | SecretEncryption | GithubApi | GithubSources | GithubAuthority | CloudStore | InngestClient | OrganizationRuntime | Polar;

const checkoutSources = Effect.fn("StoreDeployment.checkoutSources")(function* (
  store: ConfigStore, organizationId: string, deploymentId: string, runner: string,
) {
  const sources = yield* pinStoreSources(store, organizationId, deploymentId, runner);
  const githubSources = yield* GithubSources;
  const commits = new Map<string, { pin: Parameters<typeof githubSources.checkout>[0]; sources: typeof sources }>();
  for (const source of sources) {
    if (source.commit === null) return yield* new SourceUnreadable({ message: `${source.service} has no pinned commit.` });
    const pin = { ...sourceIdentity(organizationId, source), sha: source.commit };
    const key = `${pin.installationId}:${pin.repositoryId}@${pin.sha}`;
    const commit = commits.get(key);
    if (commit === undefined) commits.set(key, { pin, sources: [source] });
    else commit.sources.push(source);
  }
  const checkouts = yield* Effect.forEach(commits.values(), ({ pin, sources }) => fromGithub(githubSources.checkout(pin)).pipe(
    Effect.flatMap((repository) => Effect.forEach(sources, (source) =>
      fromGithub(sourcePaths(repository, source.root_dir, source.dockerfile_path ?? undefined)).pipe(Effect.as([source.service, repository] as const)))),
  ), { concurrency: "unbounded" });
  return Object.fromEntries(checkouts.flat());
});

/**
 * A token that only reads the contents of the Deployment's GitHub repository, for each Server to read the branch head an
 * automatic deploy rule builds from before it creates under the rule. None for a public repository, and none when
 * GitHub can't mint one: a Server then reads a public head or refuses.
 */
const headReader = Effect.fn("StoreDeployment.headReader")(function* (store: ConfigStore, deploymentId: string) {
  const sources = yield* storeTry(() => store.deploymentSources(deploymentId)).pipe(Effect.orElseSucceed(() => []));
  for (const source of sources) {
    if (source.access.type === "public") continue;
    const authority = yield* GithubAuthority;
    return yield* authority.readToken(source.access.installationId, { repositoryIds: [source.repository_id], permissions: { contents: "read" } }).pipe(
      Effect.catch((error) => Effect.logWarning("Could not mint a head reader for a Deployment.", error).pipe(Effect.as(null))),
    );
  }
  return null;
});

/**
 * Run an admitted Deployment as `runner` on the Organization's Servers, all inside the Rust SDK, with its Git Services
 * checked out at their pinned commits and its upload, while Cloud holds it, extracted. It claims first: a Deployment
 * without an upload of its own takes the one its Services last ran as it starts. Resolves to the Deployment's
 * summary, or to what the Store said when there is nothing to run; a source Cloud can't read is recorded as why nothing
 * ran; any other failure is retried. Once it has ended, the uploads it replaced go.
 */
export const runStoreDeployment = Effect.fn("StoreDeployment.run")(function* (
  data: ConfigDeploymentAdmittedEventData,
  runner: string,
  admittedAt?: number,
) {
  const store = yield* cloudStore;
  const { connections, expect } = yield* deployDialOf(data.organizationId);
  return yield* Effect.gen(function* () {
    const fetching: DeploymentStep = { name: "fetching_source", started_at: Date.now() };
    const preparing: DeploymentStep = { name: "preparing", started_at: admittedAt ?? fetching.started_at };
    const sources = yield* Effect.all({
      checkouts: checkoutSources(store, data.organizationId, data.deploymentId, runner).pipe(ended(fetching)),
      upload: storeTry(() => store.claimDeployment(data.organizationId, data.deploymentId, runner)).pipe(
        ended(preparing),
        Effect.flatMap(() => uploadOwner(store, data.organizationId, data.deploymentId)),
        Effect.flatMap((owner) => owner === undefined ? Effect.succeed(undefined) : fromGithub(extractUpload(data.organizationId, owner))),
      ),
    }).pipe(
      Effect.catchTag("SourceUnreadable", (error) => Effect.succeed({ failure: error.message })),
      Effect.ensuring(recordSteps(store, data.deploymentId, [preparing, fetching])),
    );
    const reader = yield* headReader(store, data.deploymentId);
    const withReader = "failure" in sources || reader === null ? sources : { ...sources, reader };
    return yield* storeTry(async () => ({
      ran: await store.runDeployment(data.organizationId, data.deploymentId, runner, connections, withReader, expect),
    }));
  }).pipe(
    Effect.scoped,
    // Its Deployment was replaced, cancelled or ended, another runner owns it, or this one lost track of it.
    Effect.catchIf(refusedWith("conflict"), (refused) => Effect.succeed({ nothingToRun: refused.message })),
    Effect.ensuring(released(store, data.organizationId)),
    Effect.tap(() => recheck(store, data.organizationId, data.environmentId)),
  );
});

/** `effect`, ending `step` when it succeeds: a failed one stays open, as where its Deployment stopped. */
const ended = (step: DeploymentStep) => <A, E, R>(effect: Effect.Effect<A, E, R>) =>
  Effect.tap(effect, () => Effect.sync(() => { step.ended_at = Date.now(); }));

/** Steps only time a Deploy, so one Cloud couldn't record changes nothing about it. */
const recordSteps = (store: ConfigStore, deploymentId: string, steps: DeploymentStep[]) =>
  storeTry(() => store.recordSteps(deploymentId, steps)).pipe(
    Effect.catch((error) => Effect.logWarning("Could not record a Deployment's steps.", { deploymentId, error })),
  );

/** Each Deployment a push admitted was received at `receivedAt` and reached the worker now. */
export const recordReceiving = Effect.fn("StoreDeployment.recordReceiving")(function* (deploymentIds: readonly string[], receivedAt: number) {
  const store = yield* cloudStore;
  const receiving: DeploymentStep = { name: "receiving", started_at: receivedAt, ended_at: Date.now() };
  yield* Effect.forEach(deploymentIds, (deploymentId) => recordSteps(store, deploymentId, [receiving]), { discard: true });
});

/** A Deployment's outcome may move a PR check. */
const recheck = (store: ConfigStore, organizationId: string, environmentId: string) =>
  storeTry(() => store.checks(environmentId)).pipe(
    Effect.flatMap((pulls) => requestChecks(organizationId, pulls)),
    Effect.catch((error) => Effect.logWarning("The PR checks were not requested.", { organizationId, environmentId, error })),
  );

/** Delete the uploads that can no longer build; a failure leaves them for the next run or upload to find. */
const released = (store: ConfigStore, organizationId: string) =>
  releaseUploads(store, organizationId).pipe(
    Effect.catchCause((cause) => Effect.logWarning("Keeping an upload Cloud couldn't release.", cause)),
  );

/**
 * Run `runId` stopped before it claimed its Deployment: the Store records it failed, with why, as one whose sources Cloud
 * couldn't read. One another runner claimed, or that already ended, is left as it is.
 * A duplicate run that claims it after the read races this one in the Store: whichever claims second is refused.
 */
const failUnclaimed = Effect.fn("StoreDeployment.failUnclaimed")(function* (
  store: ConfigStore, organizationId: string, deploymentId: string, runId: string, why: string,
  failure = "Cloud's worker stopped before it started this Deployment. Deploy again.",
) {
  const view = yield* storeTry(() => store.read(organizationId, { query: "deployment", id: deploymentId }));
  if (view.status !== "queued" || view.runner !== null) return { nothingToRun: why };
  // The Store refuses the claim once another attempt took its GitHub builds: that attempt runs it.
  return yield* storeTry(() => store.runDeployment(organizationId, deploymentId, storeDeploymentRunner(runId), [], { failure })).pipe(
    Effect.as<{ failed: string } | { nothingToRun: string }>({ failed: deploymentId }),
    Effect.catchIf(refusedWith("conflict"), (refused) => Effect.succeed({ nothingToRun: refused.message })),
  );
});

/**
 * Run `runId` stopped before it claimed its Deployment, because its Volumes could not move, a Server can't run it, or
 * GitHub can't take its builds: the Store records it failed with `failure`. A `restore` whose copy failed is then
 * discarded. The run is forgotten unless `keepRun`: one that still has GitHub builds to stop forgets itself after.
 */
export const failStoreDeploymentUnrun = Effect.fn("StoreDeployment.failUnrun")(function* (
  organizationId: string, environmentId: string, deploymentId: string, runId: string, failure: string, restore?: FailedRestore,
  keepRun = false,
) {
  const store = yield* cloudStore;
  const failed = yield* failUnclaimed(store, organizationId, deploymentId, runId, failure, failure).pipe(
    Effect.ensuring(released(store, organizationId)),
  );
  yield* recheck(store, organizationId, environmentId);
  if (restore !== undefined) yield* discardFailedRestore(organizationId, restore);
  yield* releasePlacementClaims(organizationId, deploymentId);
  if (!keepRun) yield* forgetStoreDeploymentRun(runId);
  return failed;
});

/** A run is one durable runner: its retried steps claim as the same runner, and a duplicate delivery is another. */
export const storeDeploymentRunner = (runId: string) => `cloud-${runId}`;

/** Record run `runId` of the worker for the Deployment, before it does anything. A retried step records it again. */
export const recordStoreDeploymentRun = Effect.fn("StoreDeployment.recordRun")(function* (
  runId: string, data: ConfigDeploymentAdmittedEventData,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(deploymentRun).values({ runId, organizationId: data.organizationId, deploymentId: data.deploymentId })
    .onConflictDoNothing();
});

/** The run ended: it has nothing left to stop. */
export const forgetStoreDeploymentRun = Effect.fn("StoreDeployment.forgetRun")(function* (runId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.delete(deploymentRun).where(eq(deploymentRun.runId, runId));
});

/**
 * Run `runId` stopped without finishing: the Store records that it stopped (or, never claimed, that it failed), so the
 * Deployment never reads running or waits queued for nobody, then its GitHub builds stop. Fails, to be retried, until
 * both are settled.
 *
 * Whether any run is cancelled is the Store's decision, in the write that ends the builds: it checks this run's
 * attempt still holds the Deployment and that the Deployment, as committed, takes no more GitHub runs, and records a
 * cancel owed for each run Cloud dispatched. Cloud cancels only those, so a stop with no answer cancels nothing, and a
 * later try still finds what is owed. A Deployment a newer attempt runs is left to it.
 */
export const stopStoreDeploymentRun = Effect.fn("StoreDeployment.stopRun")(function* (
  organizationId: string, deploymentId: string, runId: string,
) {
  const store = yield* cloudStore;
  const runner = storeDeploymentRunner(runId);
  const stopped = yield* storeTry(async () => ({ abandoned: await store.abandonDeployment(deploymentId, runner) })).pipe(
    // It never claimed it, or another runner owns it now.
    Effect.catchIf(refusedWith("conflict"), (refused) => failUnclaimed(store, organizationId, deploymentId, runId, refused.message)),
    Effect.ensuring(released(store, organizationId)),
  );
  yield* cancelStoreGithubBuilds(deploymentId, runner).pipe(Effect.catchIf(newerAttempt, () => Effect.succeed(0)));
  yield* releasePlacementClaims(organizationId, deploymentId);
  yield* forgetStoreDeploymentRun(runId);
  return stopped;
});

/**
 * Worker run `runId` begins running its Deployment, before it prepares any build: the Store numbers it after every run
 * that began before, and from then on only this run writes the Deployment's builds, taking them over from earlier
 * runs. `false` when a later run began, or another runner claimed the Deployment: this run then runs nothing.
 */
export const beginStoreDeploymentRun = Effect.fn("StoreDeployment.begin")(function* (deploymentId: string, runId: string) {
  const store = yield* cloudStore;
  return yield* storeTry(() => store.githubBegin(deploymentId, storeDeploymentRunner(runId))).pipe(
    Effect.as(true),
    Effect.catchIf(newerAttempt, () => Effect.succeed(false)),
  );
});

/** The Deployment run `runId` of the worker runs, while it is recorded. */
export const storeDeploymentRunOf = Effect.fn("StoreDeployment.runOf")(function* (runId: string) {
  const { drizzle } = yield* Database;
  const [run] = yield* drizzle.select().from(deploymentRun).where(eq(deploymentRun.runId, runId)).limit(1);
  return run === undefined ? null : { organizationId: run.organizationId, deploymentId: run.deploymentId };
});

/** How long a recorded run may sit before the sweep asks Inngest whether it still runs. */
const STALE_RUN_MS = 15 * 60_000;

/**
 * Recorded worker runs that ended without forgetting themselves: the run failed or was cancelled and its stop was
 * lost, or never handed on. Each is stopped again, once per sweep window, so a Deployment never waits on a worker
 * that is gone (queued at "Asking GitHub for a run", or owing a run its cancel). A run Inngest still reports running,
 * or can't answer for, is left.
 */
export const staleStoreDeploymentRuns = Effect.fn("StoreDeployment.staleRuns")(function* <R>(
  runStatus: (runId: string) => Effect.Effect<InngestRunStatus, unknown, R>, now: Date,
) {
  const { drizzle } = yield* Database;
  const quiet = yield* drizzle.select().from(deploymentRun).where(lt(deploymentRun.createdAt, new Date(now.getTime() - STALE_RUN_MS)));
  const sweep = Math.floor(now.getTime() / STALE_RUN_MS);
  const stale: { organizationId: string; deploymentId: string; runId: string; round: number; sweep: number }[] = [];
  for (const run of quiet) {
    const status = yield* runStatus(run.runId).pipe(Effect.option);
    if (Option.isNone(status) || status.value === "running") continue;
    stale.push({ organizationId: run.organizationId, deploymentId: run.deploymentId, runId: run.runId, round: 0, sweep });
  }
  return stale;
});

/** How long an admission has to reach its worker before Cloud hands it over again. */
const UNCLAIMED_AFTER_SECONDS = 60;

/**
 * Queued Deployments whose hand-off to the worker looks lost, in every Organization: the Store's unclaimed ones admitted
 * over a minute ago, less those in an Environment where a recorded worker run holds a Deployment still in flight (a run
 * records itself first, so one waiting its turn or walking its GitHub builds holds its Environment's queue; a stale
 * record of an ended one doesn't). A lost send or a run dropped across a Cloud redeploy stalls there.
 * ponytail: reads each recorded run's Deployment; runs are few (one per Deployment in flight).
 */
export const unclaimedStoreDeployments = Effect.fn("StoreDeployment.unclaimed")(function* (now: Date) {
  const store = yield* cloudStore;
  const before = Math.floor(now.getTime() / 1000) - UNCLAIMED_AFTER_SECONDS;
  const unclaimed = yield* storeTry(() => store.unclaimed(before));
  if (unclaimed.length === 0) return [];
  const { drizzle } = yield* Database;
  const runs = yield* drizzle.select().from(deploymentRun);
  const held = new Set<string>();
  for (const run of runs) {
    const view = yield* storeTry(() => store.read(run.organizationId, { query: "deployment", id: run.deploymentId })).pipe(Effect.option);
    if (Option.isSome(view) && view.value.in_flight) held.add(view.value.environment.id);
  }
  return unclaimed.filter((found) => !held.has(found.environment)).map((found): ConfigDeploymentAdmittedEventData => ({
    organizationId: found.organization, environmentId: found.environment, deploymentId: found.deployment, trigger: "user",
  }));
});

/** The Organization's Environments with published (Saved) state: what a first Server runs as soon as it joins. */
export const publishedEnvironments = Effect.fn("StoreDeployment.published")(function* (organizationId: string) {
  const { projects } = yield* readStore(organizationId, { query: "projects" });
  const environments = projects.flatMap((project) => project.environments.map((environment) => ({ project: project.name, environment })));
  const saved = yield* Effect.forEach(environments, (environment) =>
    readStore(organizationId, { query: "diff", environment }).pipe(Effect.map((diff) => diff.saved === null ? [] : [environment])));
  return saved.flat();
});

/**
 * Deploy `environment` to the Organization's first Server as Deployment `id`, as its Deploy button would; no member
 * admits it and nothing uploads. `services` narrows it to those Services, as the Deploy button's "only these" does;
 * empty deploys the whole Environment. A refusal (a Deploy that would delete data asks first) leaves it for the user.
 */
export const deployToFirstServer = Effect.fn("StoreDeployment.deployToFirstServer")(function* (
  organizationId: string, environment: { project: string; environment: string }, id: string, services: readonly string[] = [],
) {
  const result = yield* callStore(organizationId, null, { operation: "write", command: admitDeploy(environment, id, { services }) });
  if (result.ok) return { admitted: true };
  yield* Effect.logInfo(`Not deploying ${environment.project}/${environment.environment} to the first Server: ${result.refusal.message}`);
  return { refused: result.refusal.code, message: result.refusal.message };
});
