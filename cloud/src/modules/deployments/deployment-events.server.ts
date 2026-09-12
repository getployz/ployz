import "@tanstack/react-start/server-only";
import { and, asc, eq, gt } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { environmentDeployment, environmentDeploymentEvent } from "./tables";

/** Logs are fetched only when opened. Current progress lives on the deployment. */
export const loadDeploymentEvents = Effect.fn("Deployments.events")(function* (input: {
  organizationId: string; deploymentId: string; after: number;
}) {
  const { drizzle } = yield* Database;
  const [deployment] = yield* drizzle.select({ id: environmentDeployment.id })
    .from(environmentDeployment).where(and(eq(environmentDeployment.id, input.deploymentId), eq(environmentDeployment.organizationId, input.organizationId))).limit(1);
  if (!deployment) return yield* new NotFound({ message: "Deployment was not found." });
  const events = yield* drizzle.select().from(environmentDeploymentEvent)
    .where(and(eq(environmentDeploymentEvent.deploymentId, input.deploymentId), gt(environmentDeploymentEvent.id, input.after)))
    .orderBy(asc(environmentDeploymentEvent.id)).limit(50);
  const last = events.at(-1);
  return { events, nextSequence: events.length === 50 && last ? String(last.id) : null };
});

export const persistDeploymentProgress = Effect.fn("Deployments.persistProgress")(function* (deploymentId: string, progress: typeof environmentDeploymentEvent.$inferInsert.progress) {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.update(environmentDeployment).set({ runtimeProgress: progress, updatedAt: new Date() })
      .where(eq(environmentDeployment.id, deploymentId));
    yield* drizzle.insert(environmentDeploymentEvent).values({ deploymentId, progress });
  }));
});
