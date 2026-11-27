import "@tanstack/react-start/server-only";
import type { ConfigStore } from "@ployz/sdk";
import { Effect, Option } from "effect";
import { cloudStore, deployDialOf, refusedWith, storeTry } from "#/modules/config-store/store-sdk.server";
import { callStore, readStore, requestChecks } from "#/modules/config-store/config-store.server";
import { cancelStoreGithubBuilds } from "#/modules/config-store/store-github-builds.server";
import { fromGithub, pinStoreSources, sourceIdentity, SourceUnreadable } from "#/modules/config-store/store-sources.server";
import { deploymentRun } from "#/modules/config-store/tables";
import { discardFailedRestore, type FailedRestore, releasePlacementClaims } from "#/modules/volume-placement/volume-placement.deploy.server";
import { extractUpload, releaseUploads, uploadOwner } from "#/modules/config-store/upload.server";
import { eq } from "drizzle-orm";
import { GithubSourceError, resolveSourcePaths } from "#/modules/github/github-source.server";
import { GithubSources } from "#/modules/github/github-sources.server";
import { GithubAuthority } from "#/modules/github/github-authority.server";
import type { ConfigDeploymentAdmittedEventData } from "#/modules/config-store/config-store.events";
import { Database } from "#/server/database.server";
import type { InngestClient } from "#/modules/inngest/client";
import type { Polar } from "#/modules/billing/polar-provider.server";
import type { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { GithubApi } from "#/modules/github/github-observation.api";
import type { AppConfig } from "#/server/config.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";
import type { CloudStore } from "#/modules/config-store/store-sdk.server";
import { admitDeploy } from "./admit-deploy";

/** What Cloud's Store workers need from Cloud. */
export type StoreServices = AppConfig | Database | SecretEncryption | GithubApi | GithubSources | GithubAuthority | CloudStore | InngestClient | OrganizationRuntime | Polar;

/**
 * Pin the Deployment's Git Services, then check out each pinned repository once for the runner until the scope closes.
 * Every Service at one commit shares its checkout, with its own root and Dockerfile checked inside it.
 */
const checkoutSources = Effect.fn("StoreDeployment.checkoutSources")(function* (
  store: ConfigStore, organizationId: string, deploymentId: string,
) {
  const sources = yield* pinStoreSources(store, organizationId, deploymentId);
  const unpinned = sources.find((source) => source.commit === null);
  if (unpinned !== undefined) return yield* new SourceUnreadable({ message: `${unpinned.service} has no pinned commit.` });
  const githubSources = yield* GithubSources;
  const pins = new Map(sources.map((source) => {
    const pin = { ...sourceIdentity(organizationId, source), sha: source.commit ?? "" };
    return [`${pin.repositoryId}@${pin.sha}`, pin] as const;
  }));
  const repositories = new Map(yield* Effect.forEach(pins, ([key, pin]) =>
    fromGithub(githubSources.checkout(pin)).pipe(Effect.map((directory) => [key, directory] as const)), { concurrency: "unbounded" }));
  const checkouts: Record<string, string> = {};
  for (const source of sources) {
    const repository = repositories.get(`${source.repository_id}@${source.commit}`) ?? "";
    yield* fromGithub(Effect.tryPromise({
      try: () => resolveSourcePaths(repository, source.root_dir, source.dockerfile_path ?? undefined),
      catch: (error) => error instanceof GithubSourceError ? error : new GithubSourceError({ message: "Could not acquire the pinned repository source." }),
    }));
    checkouts[source.service] = repository;
  }
  return checkouts;
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
) {
  const store = yield* cloudStore;
  const { connections, expect } = yield* deployDialOf(data.organizationId);
  return yield* Effect.gen(function* () {
    const sources = yield* Effect.all({
      checkouts: checkoutSources(store, data.organizationId, data.deploymentId),
      upload: storeTry(() => store.claimDeployment(data.organizationId, data.deploymentId, runner)).pipe(
        Effect.flatMap(() => uploadOwner(store, data.organizationId, data.deploymentId)),
        Effect.flatMap((owner) => owner === undefined ? Effect.succeed(undefined) : fromGithub(extractUpload(data.organizationId, owner))),
      ),
    }).pipe(Effect.catchTag("SourceUnreadable", (error) => Effect.succeed({ failure: error.message })));
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
  yield* storeTry(() => store.runDeployment(organizationId, deploymentId, storeDeploymentRunner(runId), [], { failure }));
  return { failed: deploymentId };
});

/**
 * Run `runId` stopped before it claimed its Deployment, because its Volumes could not move or a Server can't run it:
 * the Store records it failed with `failure`. A `restore` whose copy failed is then discarded.
 */
export const failStoreDeploymentUnrun = Effect.fn("StoreDeployment.failUnrun")(function* (
  organizationId: string, environmentId: string, deploymentId: string, runId: string, failure: string, restore?: FailedRestore,
) {
  const store = yield* cloudStore;
  const failed = yield* failUnclaimed(store, organizationId, deploymentId, runId, failure, failure).pipe(
    Effect.ensuring(released(store, organizationId)),
  );
  yield* recheck(store, organizationId, environmentId);
  if (restore !== undefined) yield* discardFailedRestore(organizationId, restore);
  yield* releasePlacementClaims(organizationId, deploymentId);
  yield* forgetStoreDeploymentRun(runId);
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
 * Run `runId` stopped without finishing: its Deployment's GitHub builds stop and the Store records that it stopped (or,
 * never claimed, that it failed), each whether or not the other could, so the Deployment never reads running or
 * waits queued for nobody. Fails, to be retried, if either failed.
 */
export const stopStoreDeploymentRun = Effect.fn("StoreDeployment.stopRun")(function* (
  organizationId: string, deploymentId: string, runId: string,
) {
  const store = yield* cloudStore;
  const cancelled = yield* Effect.exit(cancelStoreGithubBuilds(deploymentId));
  const stopped = yield* storeTry(async () => ({ abandoned: await store.abandonDeployment(deploymentId, storeDeploymentRunner(runId)) })).pipe(
    // It never claimed it, or another runner owns it now.
    Effect.catchIf(refusedWith("conflict"), (refused) => failUnclaimed(store, organizationId, deploymentId, runId, refused.message)),
    Effect.ensuring(released(store, organizationId)),
  );
  yield* releasePlacementClaims(organizationId, deploymentId);
  yield* cancelled;
  yield* forgetStoreDeploymentRun(runId);
  return stopped;
});

/** Inngest cancelled run `runId`: stop what it ran, if it is a run of the worker. */
export const cancelStoreDeploymentRun = Effect.fn("StoreDeployment.cancelRun")(function* (runId: string) {
  const { drizzle } = yield* Database;
  const [run] = yield* drizzle.select().from(deploymentRun).where(eq(deploymentRun.runId, runId)).limit(1);
  if (run === undefined) return { skipped: true };
  return yield* stopStoreDeploymentRun(run.organizationId, run.deploymentId, runId);
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
    organizationId: found.organization, environmentId: found.environment, deploymentId: found.deployment, number: found.number,
    trigger: "user",
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
