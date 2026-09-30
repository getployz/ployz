import "@tanstack/react-start/server-only";
import type { ConfigStore, DeploymentStatus, GitSource } from "@ployz/sdk";
import { Data, Effect, Option } from "effect";
import type { InngestClient } from "#/modules/inngest/client";
import type { Polar } from "#/modules/billing/polar-provider.server";
import type { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { cloudStore, refusedWith, type CloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { cancelStoreGithubBuilds, connectionsOf, requestChecks } from "#/modules/config-store/config-store.server";
import { deploymentRun } from "#/modules/config-store/tables";
import { extractUpload, releaseUpload } from "#/modules/config-store/upload.server";
import { eq } from "drizzle-orm";
import type { GithubApi } from "#/modules/github/github-observation.api";
import { GithubSourceError, materializeGithubSource, resolveGithubSourceSha } from "#/modules/github/github-source.server";
import type { ConfigDeploymentAdmittedEventData } from "#/modules/inngest/events";
import type { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";

/** What Cloud's Store workers need from Cloud. */
export type StoreServices = AppConfig | Database | SecretEncryption | GithubApi | CloudStore | InngestClient | OrganizationRuntime | Polar;

/** Runs one of the Store workers' Effects for an Inngest step. */
export type StoreEffectRunner = <A, E extends Error>(program: Effect.Effect<A, E, StoreServices>) => Promise<A>;

/** Cloud could not read a Git Service's source; users read the message. */
export class SourceUnreadable extends Data.TaggedError("SourceUnreadable")<{ readonly message: string }> {}

const fromGithub = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.mapError((error) => new SourceUnreadable({
  message: error instanceof GithubSourceError ? error.message : "GitHub didn't answer while reading the source. Deploy again.",
})));

const identity = (organizationId: string, source: GitSource) => ({
  organizationId,
  installationId: source.access.type === "public" ? null : source.access.installationId,
  repositoryId: source.repository_id,
});

/**
 * Pin each Git Service of the Deployment to its branch's head, unless it is pinned already (a pin never moves, so a
 * retried run reads the same commit). Resolves to every source with its pin.
 */
export const pinStoreSources = Effect.fn("StoreDeployment.pinSources")(function* (
  store: ConfigStore, organizationId: string, deploymentId: string,
) {
  const sources = yield* storeTry(() => store.deploymentSources(deploymentId));
  const heads: Record<string, string> = {};
  for (const source of sources) {
    if (source.commit !== null) continue;
    if (source.branch === null) {
      return yield* new SourceUnreadable({ message: `Reconnect ${source.service}'s branch before deploying.` });
    }
    heads[source.service] = yield* fromGithub(resolveGithubSourceSha({ ...identity(organizationId, source), branch: source.branch }));
  }
  return Object.keys(heads).length > 0 ? yield* storeTry(() => store.pinSources(deploymentId, heads)) : sources;
});

/** Pin the Deployment's Git Services, then check out every pin for the runner until the scope closes. */
const checkoutSources = Effect.fn("StoreDeployment.checkoutSources")(function* (
  store: ConfigStore, organizationId: string, deploymentId: string,
) {
  const sources = yield* pinStoreSources(store, organizationId, deploymentId);
  const checkouts: Record<string, string> = {};
  for (const source of sources) {
    if (source.commit === null) return yield* new SourceUnreadable({ message: `${source.service} has no pinned commit.` });
    const capture: Parameters<typeof materializeGithubSource>[0] = { ...identity(organizationId, source), sha: source.commit, rootDir: source.root_dir };
    if (source.dockerfile_path !== null) capture.dockerfilePath = source.dockerfile_path;
    checkouts[source.service] = (yield* fromGithub(materializeGithubSource(capture))).repositoryDirectory;
  }
  return checkouts;
});

/**
 * Run an admitted Deployment as `runner` on the Organization's Servers, all inside the Rust SDK, with its Git Services
 * checked out at their pinned commits and its upload, while Cloud holds it, extracted. Resolves to the Deployment's
 * summary, or to what the Store said when there is nothing to run; a source Cloud can't read is recorded as why nothing
 * ran; any other failure is retried. Its upload goes once it has ended.
 */
export const runStoreDeployment = Effect.fn("StoreDeployment.run")(function* (
  data: ConfigDeploymentAdmittedEventData,
  runner: string,
) {
  const store = yield* cloudStore;
  const connections = yield* connectionsOf(data.organizationId);
  return yield* Effect.gen(function* () {
    const sources = yield* Effect.all({
      checkouts: checkoutSources(store, data.organizationId, data.deploymentId),
      upload: fromGithub(extractUpload(data.organizationId, data.deploymentId)),
    }).pipe(Effect.catchTag("SourceUnreadable", (error) => Effect.succeed({ failure: error.message })));
    return yield* storeTry(async () => ({
      ran: await store.runDeployment(data.organizationId, data.deploymentId, runner, connections, sources),
    }));
  }).pipe(
    Effect.scoped,
    // Its Deployment was replaced, cancelled or ended, another runner owns it, or this one lost track of it.
    Effect.catchIf(refusedWith("conflict"), (refused) => Effect.succeed({ nothingToRun: refused.message })),
    Effect.ensuring(released(store, data.organizationId, data.deploymentId)),
    // Its outcome may move a PR check.
    Effect.tap(() => requestChecks(data.organizationId)),
  );
});

/** Delete the Deployment's upload if it has ended; a failure leaves it for the next run or upload to find. */
const released = (store: ConfigStore, organizationId: string, deploymentId: string) =>
  releaseUpload(store, organizationId, deploymentId).pipe(
    Effect.catchCause((cause) => Effect.logWarning("Keeping an upload Cloud couldn't release.", cause)),
  );

/**
 * A run that never claimed its Deployment stopped: nothing else will run it now, so it ends cancelled rather than wait
 * queued to be handed over again. One another runner claimed, or that already ended, is left as it is.
 * ponytail: read-then-cancel; a duplicate run claiming it in between is cancelled with it.
 */
const cancelUnclaimed = Effect.fn("StoreDeployment.cancelUnclaimed")(function* (
  store: ConfigStore, organizationId: string, deploymentId: string, why: string,
) {
  const view = yield* storeTry(() => store.read(organizationId, { query: "deployment", id: deploymentId }));
  if (view.status !== "queued" || view.runner !== null) return { nothingToRun: why };
  yield* storeTry(() => store.write(organizationId, { command: "cancel", deployment: deploymentId }));
  return { cancelled: deploymentId };
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
 * never claimed, that it was cancelled), each whether or not the other could, so the Deployment never reads running or
 * waits queued for nobody. Fails, to be retried, if either failed.
 */
export const stopStoreDeploymentRun = Effect.fn("StoreDeployment.stopRun")(function* (
  organizationId: string, deploymentId: string, runId: string,
) {
  const store = yield* cloudStore;
  const cancelled = yield* Effect.exit(cancelStoreGithubBuilds(organizationId, deploymentId));
  const stopped = yield* storeTry(async () => ({ abandoned: await store.abandonDeployment(deploymentId, storeDeploymentRunner(runId)) })).pipe(
    // It never claimed it, or another runner owns it now.
    Effect.catchIf(refusedWith("conflict"), (refused) => cancelUnclaimed(store, organizationId, deploymentId, refused.message)),
    Effect.ensuring(released(store, organizationId, deploymentId)),
  );
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

/** Statuses of a Deployment still in flight. */
const IN_FLIGHT: ReadonlySet<DeploymentStatus> = new Set(["queued", "running", "cancelling"]);

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
    if (Option.isSome(view) && IN_FLIGHT.has(view.value.status)) held.add(view.value.environment.id);
  }
  return unclaimed.filter((found) => !held.has(found.environment)).map((found): ConfigDeploymentAdmittedEventData => ({
    organizationId: found.organization, environmentId: found.environment, deploymentId: found.deployment,
  }));
});
