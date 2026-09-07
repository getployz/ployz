import "@tanstack/react-start/server-only";

import { and, eq, isNull } from "drizzle-orm";
import { Effect } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import {
  sendInngestEvent,
} from "#/modules/inngest/client";
import { createEnvironmentDeployRequestedEvent } from "#/modules/inngest/events";

export const ENVIRONMENT_DEPLOYMENT_DISPATCH_FAILURE_CODE =
  "inngest_dispatch_failed";
export const ENVIRONMENT_DEPLOYMENT_DISPATCH_FAILURE_MESSAGE =
  "Cloud could not dispatch the deployment workflow.";

export type EnvironmentDeploymentDispatchInput = {
  readonly environmentDeploymentId: string;
  readonly environmentId: string;
};

/**
 * Dispatches any committed queued row that is still unowned. Replays enqueue
 * deliberately: the event has a deterministic deployment ID, so Inngest owns
 * deduplication across a crash after admission commit or after event send.
 */
export const dispatchEnvironmentDeployment = Effect.fn(
  "Deployments.dispatchEnvironmentDeployment",
)(function* (input: EnvironmentDeploymentDispatchInput) {
  const { drizzle } = yield* Database;
  const requestedAt = new Date();
  const requested = yield* drizzle
    .update(schemaEnvironmentDeployment)
    .set({ dispatchRequestedAt: requestedAt, updatedAt: requestedAt })
    .where(
      and(
        eq(
          schemaEnvironmentDeployment.id,
          input.environmentDeploymentId,
        ),
        eq(schemaEnvironmentDeployment.environmentId, input.environmentId),
        eq(schemaEnvironmentDeployment.status, "queued"),
        isNull(schemaEnvironmentDeployment.inngestRunId),
      ),
    )
    .returning({ id: schemaEnvironmentDeployment.id });
  if (requested.length === 0) {
    return yield* new Conflict({
      message: "The queued deployment is no longer available to dispatch.",
    });
  }

  yield* sendInngestEvent(createEnvironmentDeployRequestedEvent(input)).pipe(
    Effect.catchTag("InngestEventSendError", (failure) =>
      Effect.gen(function* () {
        const finishedAt = new Date();
        yield* drizzle
          .update(schemaEnvironmentDeployment)
          .set({
            status: "failed",
            failureCode: ENVIRONMENT_DEPLOYMENT_DISPATCH_FAILURE_CODE,
            failureMessage: ENVIRONMENT_DEPLOYMENT_DISPATCH_FAILURE_MESSAGE,
            finishedAt,
            updatedAt: finishedAt,
          })
          .where(
            and(
              eq(
                schemaEnvironmentDeployment.id,
                input.environmentDeploymentId,
              ),
              eq(
                schemaEnvironmentDeployment.environmentId,
                input.environmentId,
              ),
              eq(schemaEnvironmentDeployment.status, "queued"),
              isNull(schemaEnvironmentDeployment.inngestRunId),
            ),
          )
          .returning({ id: schemaEnvironmentDeployment.id });
        return yield* failure;
      }),
    ),
  );
  return { state: "dispatched" as const };
});
