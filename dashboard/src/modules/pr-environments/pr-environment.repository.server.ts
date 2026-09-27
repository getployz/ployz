import "@tanstack/react-start/server-only";
import { and, eq, isNotNull } from "drizzle-orm";
import { Effect } from "effect";
import { environmentBranch } from "#/modules/project/tables";
import { Database } from "#/server/database.server";

/** Whether the Environment is a PR Environment: a Branch recording a pull request. */
export const isPrEnvironment = Effect.fn("PrEnvironments.isPrEnvironment")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ environmentId: environmentBranch.environmentId }).from(environmentBranch)
    .where(and(eq(environmentBranch.environmentId, environmentId), isNotNull(environmentBranch.prNumber)));
  return row !== undefined;
});
