import "@tanstack/react-start/server-only";
import { Data, Effect } from "effect";
import { cloudStore } from "#/modules/config-store/config-store.server";
import type { ConfigDeploymentAdmittedEventData } from "#/modules/inngest/events";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import type { AppConfig } from "#/server/config.server";
import type { Database } from "#/server/database.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";

/** What running a Store Deployment needs from Cloud. */
export type StoreDeploymentServices = AppConfig | Database | SecretEncryption;

export class StoreDeploymentRunFailure extends Data.TaggedError("StoreDeploymentRunFailure")<{ readonly cause: unknown }> {}

/** The Store's refusal when this runner has nothing to run: its Deployment was replaced, cancelled or ended, another
 * runner owns it, or this one lost track of it after preparing it. */
function isConflict(cause: unknown): cause is { readonly message: string } {
  return typeof cause === "object" && cause !== null && "code" in cause && cause.code === "conflict";
}

/**
 * Run an admitted Deployment as `runner` on the Organization's Servers, all inside the Rust SDK. Resolves to the
 * Deployment's summary, or to what the Store said when there is nothing to run; any other failure is retried.
 */
export const runStoreDeployment = Effect.fn("StoreDeployment.run")(function* (
  data: ConfigDeploymentAdmittedEventData,
  runner: string,
) {
  const store = yield* cloudStore;
  const loaded = yield* loadOrganizationConnections(data.organizationId);
  const connections = loaded.kind === "ready" ? loaded.connections : [];
  return yield* Effect.tryPromise({
    try: async () => ({ ran: await store.runDeployment(data.organizationId, data.deploymentId, runner, connections) }),
    catch: (cause) => cause,
  }).pipe(
    Effect.catch((cause) => isConflict(cause)
      ? Effect.succeed({ nothingToRun: cause.message })
      : Effect.fail(new StoreDeploymentRunFailure({ cause }))),
  );
});

/** Every attempt of `runner` failed: the Store records that it stopped, so the Deployment never reads running. */
export const abandonStoreDeployment = Effect.fn("StoreDeployment.abandon")(function* (deploymentId: string, runner: string) {
  const store = yield* cloudStore;
  return yield* Effect.tryPromise({
    try: async () => ({ abandoned: await store.abandonDeployment(deploymentId, runner) }),
    catch: (cause) => cause,
  }).pipe(
    // It never claimed it, or another runner owns it now.
    Effect.catch((cause) => isConflict(cause)
      ? Effect.succeed({ nothingToRun: cause.message })
      : Effect.fail(new StoreDeploymentRunFailure({ cause }))),
  );
});
