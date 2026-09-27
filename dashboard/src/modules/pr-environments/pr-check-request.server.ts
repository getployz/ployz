import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createPrCheckRequestedEvent } from "#/modules/inngest/events";
import { environmentBranch } from "#/modules/project/tables";
import { afterDatabaseCommit, Database } from "#/server/database.server";
import { isPrEnvironment } from "./pr-environment.repository.server";

/**
 * Asks for a PR Environment's check to be posted again once the change commits. The poster reads the state it posts,
 * so requests only need to follow changes. A failed request is logged: the change itself stands.
 */
export const requestPrCheck = (prEnvironmentIds: string[]) => prEnvironmentIds.length === 0 ? Effect.void : afterDatabaseCommit(
  sendInngestEvent(prEnvironmentIds.map((prEnvironmentId) => createPrCheckRequestedEvent({ prEnvironmentId }))).pipe(
    Effect.catch((error) => Effect.logWarning("The PR check was not requested.", { prEnvironmentIds, error })),
  ),
);

/** `requestPrCheck` when the Environment is a PR Environment: its settings changed. */
export const requestPrCheckIfPrEnvironment = Effect.fn("PrEnvironments.requestPrCheckIfPrEnvironment")(function* (environmentId: string) {
  if (yield* isPrEnvironment(environmentId)) yield* requestPrCheck([environmentId]);
});

/** `requestPrCheck` for every PR Environment of one pull request. */
export const requestPullRequestChecks = Effect.fn("PrEnvironments.requestPullRequestChecks")(function* (repositoryId: number, number: number) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ id: environmentBranch.environmentId }).from(environmentBranch)
    .where(and(eq(environmentBranch.prRepositoryId, repositoryId), eq(environmentBranch.prNumber, number)));
  yield* requestPrCheck(rows.map((row) => row.id));
});
