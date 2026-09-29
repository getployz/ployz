import "@tanstack/react-start/server-only";
import type { ConfigStore, GitSource } from "@ployz/sdk";
import { Data, Effect } from "effect";
import type { InngestClient } from "#/modules/inngest/client";
import type { Polar } from "#/modules/billing/polar-provider.server";
import type { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { cloudStore, refusedWith, type CloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { cancelStoreGithubBuilds, requestChecks } from "#/modules/config-store/config-store.server";
import { deploymentRun } from "#/modules/config-store/tables";
import { extractUpload, releaseUpload } from "#/modules/config-store/upload.server";
import { eq, sql } from "drizzle-orm";
import type { GithubApi } from "#/modules/github/github-observation.api";
import { GithubSourceError, materializeGithubSource, resolveGithubSourceSha } from "#/modules/github/github-source.server";
import type { ConfigDeploymentAdmittedEventData } from "#/modules/inngest/events";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
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
  const loaded = yield* loadOrganizationConnections(data.organizationId);
  const connections = loaded.kind === "ready" ? loaded.connections : [];
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

/** Every attempt of `runner` failed: the Store records that it stopped, so the Deployment never reads running. */
export const abandonStoreDeployment = Effect.fn("StoreDeployment.abandon")(function* (
  organizationId: string, deploymentId: string, runner: string,
) {
  const store = yield* cloudStore;
  return yield* storeTry(async () => ({ abandoned: await store.abandonDeployment(deploymentId, runner) })).pipe(
    // It never claimed it, or another runner owns it now.
    Effect.catchIf(refusedWith("conflict"), (refused) => Effect.succeed({ nothingToRun: refused.message })),
    Effect.ensuring(released(store, organizationId, deploymentId)),
  );
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
 * Run `runId` stopped without finishing: its Deployment's GitHub builds stop and the Store records that it stopped,
 * each whether or not the other could, so the Deployment never reads running. Fails, to be retried, if either failed.
 */
export const stopStoreDeploymentRun = Effect.fn("StoreDeployment.stopRun")(function* (
  organizationId: string, deploymentId: string, runId: string,
) {
  const cancelled = yield* Effect.exit(cancelStoreGithubBuilds(organizationId, deploymentId));
  const abandoned = yield* abandonStoreDeployment(organizationId, deploymentId, storeDeploymentRunner(runId));
  yield* cancelled;
  yield* forgetStoreDeploymentRun(runId);
  return abandoned;
});

/** Inngest cancelled run `runId`: stop what it ran, if it is a run of the worker. */
export const cancelStoreDeploymentRun = Effect.fn("StoreDeployment.cancelRun")(function* (runId: string) {
  const { drizzle } = yield* Database;
  const [run] = yield* drizzle.select().from(deploymentRun).where(eq(deploymentRun.runId, runId)).limit(1);
  if (run === undefined) return { skipped: true };
  return yield* stopStoreDeploymentRun(run.organizationId, run.deploymentId, runId);
});

/** How long an admission has to reach its worker before the sweep hands it over again. */
const UNCLAIMED_AFTER_SECONDS = 10 * 60;

/**
 * Queued Deployments no runner claimed since well after their admission, in every Organization: the hand-off to the
 * worker may have been lost (a failed send, a crash after the commit). Handing one over again sends its admission's
 * own event, which Inngest drops while that one's run still waits its turn.
 * ponytail: reads the Store's table directly until the Store answers this itself.
 */
export const unclaimedStoreDeployments = Effect.fn("StoreDeployment.unclaimed")(function* (now: Date) {
  const { drizzle } = yield* Database;
  const before = Math.floor(now.getTime() / 1000) - UNCLAIMED_AFTER_SECONDS;
  const rows = yield* drizzle.execute<{ id: string; organization_id: string; environment_id: string }>(sql`
    select id, organization_id, environment_id from config_deployment where status = 'queued' and admitted < ${before}`, "objects");
  return rows.map((row): ConfigDeploymentAdmittedEventData => ({
    organizationId: row.organization_id, environmentId: row.environment_id, deploymentId: row.id,
  }));
});
