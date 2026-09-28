import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createPrCheckRequestedEvent } from "#/modules/inngest/events";
import { environment } from "#/modules/project/tables";
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

/**
 * `requestPullRequestChecks` for every current PR Environment in the Environment's project: a PR Environment's settings
 * or approvals feed its own check, and any other Environment may be a Destination whose state or tracking feeds theirs.
 */
export const requestPrCheck = Effect.fn("PrEnvironments.requestPrCheck")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ projectId: environment.projectId }).from(environment).where(eq(environment.id, environmentId));
  if (row) yield* requestProjectPrChecks(row.projectId);
});

/** `requestPullRequestChecks` for every current PR Environment in the project, as when one of its Environments goes. */
export const requestProjectPrChecks = Effect.fn("PrEnvironments.requestProjectPrChecks")(function* (projectId: string) {
  const { drizzle } = yield* Database;
  // ponytail: every PR Environment of the project, not just those the Environment is a Destination of.
  const pullRequests = yield* drizzle.selectDistinct({ repositoryId: prEnvironment.repositoryId, number: prEnvironment.number })
    .from(prEnvironment).where(and(eq(prEnvironment.projectId, projectId), eq(prEnvironment.retired, false)));
  for (const { repositoryId, number } of pullRequests) yield* requestPullRequestChecks(repositoryId, number);
});
