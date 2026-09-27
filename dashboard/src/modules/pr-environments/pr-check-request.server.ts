import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createPrCheckRequestedEvent } from "#/modules/inngest/events";
import { afterDatabaseCommit, Database } from "#/server/database.server";
import { prEnvironment } from "./tables";

/**
 * Asks for a pull request's check to be posted again once the change commits. The poster reads the state it posts,
 * so requests only need to follow changes. A failed request is logged: the change itself stands.
 */
export const requestPullRequestChecks = (repositoryId: number, number: number) => afterDatabaseCommit(
  sendInngestEvent(createPrCheckRequestedEvent({ repositoryId, number })).pipe(
    Effect.catch((error) => Effect.logWarning("The PR check was not requested.", { repositoryId, number, error })),
  ),
);

/** `requestPullRequestChecks` when the Environment is a PR Environment: its settings or approvals changed. */
export const requestPrCheck = Effect.fn("PrEnvironments.requestPrCheck")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [pullRequest] = yield* drizzle.select({ repositoryId: prEnvironment.repositoryId, number: prEnvironment.number })
    .from(prEnvironment).where(eq(prEnvironment.environmentId, environmentId));
  if (pullRequest) yield* requestPullRequestChecks(pullRequest.repositoryId, pullRequest.number);
});
