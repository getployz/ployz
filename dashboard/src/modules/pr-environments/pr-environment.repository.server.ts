import "@tanstack/react-start/server-only";
import { desc, eq } from "drizzle-orm";
import { Effect } from "effect";
import { environmentSavedStateSnapshot } from "#/modules/deployments/tables";
import { decodePersistedSavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { environment } from "#/modules/project/tables";
import { Database } from "#/server/database.server";
import { destinations } from "./destinations";
import { prEnvironment } from "./tables";

/** Whether the Environment is a PR Environment. */
export const isPrEnvironment = Effect.fn("PrEnvironments.isPrEnvironment")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ environmentId: prEnvironment.environmentId }).from(prEnvironment)
    .where(eq(prEnvironment.environmentId, environmentId));
  return row !== undefined;
});

/**
 * A PR Environment's Destinations: the Destinations rule over its pull request's recorded target Git branch and each of
 * the project's Environments' latest Saved State. Empty for an Environment that isn't a PR Environment.
 */
export const prDestinations = Effect.fn("PrEnvironments.prDestinations")(function* (prEnvironmentId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(prEnvironment).where(eq(prEnvironment.environmentId, prEnvironmentId));
  if (!row) return [];
  const latest = yield* drizzle.selectDistinctOn([environmentSavedStateSnapshot.environmentId], {
    id: environmentSavedStateSnapshot.environmentId, intent: environmentSavedStateSnapshot.intent, prEnvironment: prEnvironment.environmentId,
  }).from(environmentSavedStateSnapshot)
    .innerJoin(environment, eq(environment.id, environmentSavedStateSnapshot.environmentId))
    .leftJoin(prEnvironment, eq(prEnvironment.environmentId, environment.id))
    .where(eq(environment.projectId, row.projectId))
    .orderBy(environmentSavedStateSnapshot.environmentId, desc(environmentSavedStateSnapshot.createdAt), desc(environmentSavedStateSnapshot.id));
  const environments = yield* Effect.forEach(latest, (saved) => decodePersistedSavedEnvironmentIntent(saved.intent).pipe(
    Effect.map((intent) => ({ id: saved.id, prEnvironment: saved.prEnvironment !== null, savedServices: intent.services.map((node) => node.config) })),
  ));
  return destinations({ environments, repositoryId: row.repositoryId, targetBranch: row.targetBranch });
});
