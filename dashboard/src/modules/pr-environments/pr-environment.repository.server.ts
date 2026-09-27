import "@tanstack/react-start/server-only";
import { and, desc, eq, isNotNull } from "drizzle-orm";
import { Effect } from "effect";
import { environmentSavedStateSnapshot } from "#/modules/deployments/tables";
import { decodePersistedSavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { environment, environmentBranch } from "#/modules/project/tables";
import { Database } from "#/server/database.server";
import { destinations } from "./destinations";

/** Whether the Environment is a PR Environment: a Branch recording a pull request. */
export const isPrEnvironment = Effect.fn("PrEnvironments.isPrEnvironment")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ environmentId: environmentBranch.environmentId }).from(environmentBranch)
    .where(and(eq(environmentBranch.environmentId, environmentId), isNotNull(environmentBranch.prNumber)));
  return row !== undefined;
});

/**
 * A PR Environment's Destinations: the Destinations rule over its pull request's recorded target Git branch and each of
 * the project's Environments' latest Saved State. Empty for an Environment that isn't a PR Environment.
 */
export const prDestinations = Effect.fn("PrEnvironments.prDestinations")(function* (prEnvironmentId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(environmentBranch).where(eq(environmentBranch.environmentId, prEnvironmentId));
  if (!row?.prRepositoryId || !row.prTargetBranch) return [];
  const latest = yield* drizzle.selectDistinctOn([environmentSavedStateSnapshot.environmentId], {
    id: environmentSavedStateSnapshot.environmentId, intent: environmentSavedStateSnapshot.intent, prNumber: environmentBranch.prNumber,
  }).from(environmentSavedStateSnapshot)
    .innerJoin(environment, eq(environment.id, environmentSavedStateSnapshot.environmentId))
    .leftJoin(environmentBranch, eq(environmentBranch.environmentId, environment.id))
    .where(eq(environment.projectId, row.projectId))
    .orderBy(environmentSavedStateSnapshot.environmentId, desc(environmentSavedStateSnapshot.createdAt), desc(environmentSavedStateSnapshot.id));
  const environments = yield* Effect.forEach(latest, (saved) => decodePersistedSavedEnvironmentIntent(saved.intent).pipe(
    Effect.map((intent) => ({ id: saved.id, prEnvironment: saved.prNumber !== null, savedServices: intent.services.map((node) => node.config) })),
  ));
  return destinations({ environments, repositoryId: row.prRepositoryId, targetBranch: row.prTargetBranch });
});
