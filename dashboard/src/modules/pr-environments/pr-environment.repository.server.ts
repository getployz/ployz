import "@tanstack/react-start/server-only";
import { desc, eq, type SQL } from "drizzle-orm";
import { Effect } from "effect";
import { environmentSavedStateSnapshot } from "#/modules/deployments/tables";
import { decodePersistedSavedEnvironmentIntent, withoutSealedCiphertext } from "#/modules/environment-design/saved-intent";
import { environment, environmentBranch } from "#/modules/project/tables";
import { Database } from "#/server/database.server";
import { destinations } from "./destinations";
import { prEnvironment, type BranchRow } from "./tables";

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
  // Every Environment, with or without a Saved State, so each Parent chain is whole.
  const rows = yield* drizzle.selectDistinctOn([environment.id], {
    id: environment.id, intent: environmentSavedStateSnapshot.intent, prEnvironment: prEnvironment.environmentId,
    parentId: environmentBranch.parentEnvironmentId,
  }).from(environment)
    .leftJoin(environmentSavedStateSnapshot, eq(environmentSavedStateSnapshot.environmentId, environment.id))
    .leftJoin(environmentBranch, eq(environmentBranch.environmentId, environment.id))
    .leftJoin(prEnvironment, eq(prEnvironment.environmentId, environment.id))
    .where(eq(environment.projectId, row.projectId))
    .orderBy(environment.id, desc(environmentSavedStateSnapshot.createdAt), desc(environmentSavedStateSnapshot.id));
  const environments = yield* Effect.forEach(rows, (row) => Effect.gen(function* () {
    const savedServices = row.intent === null ? [] : (yield* decodePersistedSavedEnvironmentIntent(row.intent)).services.map((node) => node.config);
    return { id: row.id, parentId: row.parentId, prEnvironment: row.prEnvironment !== null, savedServices };
  }));
  return destinations({ environments, repositoryId: row.repositoryId, targetBranch: row.targetBranch });
});

/** Branch rows as the browser has them: redacted base, and a PR Environment's pull request. */
export const loadBranchRows = Effect.fn("PrEnvironments.loadBranchRows")(function* (where: SQL | undefined) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ branch: environmentBranch, pullRequest: prEnvironment }).from(environmentBranch)
    .leftJoin(prEnvironment, eq(prEnvironment.environmentId, environmentBranch.environmentId)).where(where);
  return rows.map(({ branch, pullRequest }): BranchRow => ({ ...branch, base: withoutSealedCiphertext(branch.base), pullRequest }));
});
