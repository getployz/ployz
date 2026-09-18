import "@tanstack/react-start/server-only";
import { and, eq, sql } from "drizzle-orm";
import { Effect } from "effect";
import { environmentDeployment } from "./tables";
import { cancelDeploymentBeforeExecution } from "./runtime-lifecycle.repository.server";
import { Database } from "#/server/database.server";

export const markCancelledByInngestRunId = Effect.fn("Deployments.markCancelledByInngestRunId")(
  (runId: string, message = "Cancelled in Inngest.") => markDeploymentCancelled({ runId }, message),
);

export const markDeploymentCancelled = Effect.fn("Deployments.markDeploymentCancelled")(
  function* (target: { runId: string } | { deploymentId: string }, message: string) {
    const { drizzle } = yield* Database;
    const [deployment] = yield* drizzle.select({ id: environmentDeployment.id }).from(environmentDeployment)
      .where("runId" in target ? eq(environmentDeployment.inngestRunId, target.runId)
        : eq(environmentDeployment.id, target.deploymentId)).limit(1);
    if (!deployment) return false;
    const expectedInngestRunId = "runId" in target ? target.runId : undefined;
    const cancelled = yield* cancelDeploymentBeforeExecution({
      environmentDeploymentId: deployment.id, expectedInngestRunId,
      message,
    });
    if (cancelled) return true;
    // Execution retains its slot until the SDK reports cleanup and its outcome.
    yield* drizzle.update(environmentDeployment).set({
      cancellationRequestedAt: sql`coalesce(${environmentDeployment.cancellationRequestedAt}, now())`,
      updatedAt: new Date(),
    }).where(and(eq(environmentDeployment.id, deployment.id), eq(environmentDeployment.status, "deploying"),
      expectedInngestRunId ? eq(environmentDeployment.inngestRunId, expectedInngestRunId) : undefined));
    return false;
  },
);

export const requestDeploymentCancellation = Effect.fn("Deployments.requestDeploymentCancellation")(
  (deploymentId: string) => markDeploymentCancelled({ deploymentId }, "Cancelled before runtime execution."),
);
