import "@tanstack/react-start/server-only";
import type { ConfigStore, GitSource } from "@ployz/sdk";
import { Data, Effect } from "effect";
import { cloudStore } from "#/modules/config-store/config-store.server";
import { extractUpload, releaseUpload } from "#/modules/config-store/upload.server";
import type { GithubApi } from "#/modules/github/github-observation.api";
import { GithubSourceError, materializeGithubSource, resolveGithubSourceSha } from "#/modules/github/github-source.server";
import type { ConfigDeploymentAdmittedEventData } from "#/modules/inngest/events";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import type { AppConfig } from "#/server/config.server";
import type { Database } from "#/server/database.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";

/** What running a Store Deployment needs from Cloud. */
export type StoreDeploymentServices = AppConfig | Database | SecretEncryption | GithubApi;

export class StoreDeploymentRunFailure extends Data.TaggedError("StoreDeploymentRunFailure")<{ readonly cause: unknown }> {}

/** Cloud could not read a Git Service's source; users read the message. */
export class SourceUnreadable extends Data.TaggedError("SourceUnreadable")<{ readonly message: string }> {}

const storeCall = <A>(call: () => Promise<A>) =>
  Effect.tryPromise({ try: call, catch: (cause) => new StoreDeploymentRunFailure({ cause }) });

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
  const sources = yield* storeCall(() => store.deploymentSources(deploymentId));
  const heads: Record<string, string> = {};
  for (const source of sources) {
    if (source.commit !== null) continue;
    if (source.branch === null) {
      return yield* new SourceUnreadable({ message: `Reconnect ${source.service}'s branch before deploying.` });
    }
    heads[source.service] = yield* fromGithub(resolveGithubSourceSha({ ...identity(organizationId, source), branch: source.branch }));
  }
  return Object.keys(heads).length > 0 ? yield* storeCall(() => store.pinSources(deploymentId, heads)) : sources;
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

/** The Store's refusal when this runner has nothing to run: its Deployment was replaced, cancelled or ended, another
 * runner owns it, or this one lost track of it after preparing it. */
export function isConflict(cause: unknown): cause is { readonly message: string } {
  return typeof cause === "object" && cause !== null && "code" in cause && cause.code === "conflict";
}

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
    return yield* storeCall(async () => ({
      ran: await store.runDeployment(data.organizationId, data.deploymentId, runner, connections, sources),
    }));
  }).pipe(
    Effect.scoped,
    Effect.catchTag("StoreDeploymentRunFailure", (error) => isConflict(error.cause)
      ? Effect.succeed({ nothingToRun: error.cause.message })
      : Effect.fail(error)),
    Effect.ensuring(released(store, data.organizationId, data.deploymentId)),
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
  return yield* Effect.tryPromise({
    try: async () => ({ abandoned: await store.abandonDeployment(deploymentId, runner) }),
    catch: (cause) => cause,
  }).pipe(
    // It never claimed it, or another runner owns it now.
    Effect.catch((cause) => isConflict(cause)
      ? Effect.succeed({ nothingToRun: cause.message })
      : Effect.fail(new StoreDeploymentRunFailure({ cause }))),
    Effect.ensuring(released(store, organizationId, deploymentId)),
  );
});
