import "@tanstack/react-start/server-only";
import { and, asc, eq, gt, sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { environmentDeployment, environmentDeploymentBuildOutput, environmentDeploymentBuildStep, environmentDeploymentEvent } from "./tables";
import type { BuildOutputWrite, BuildStepWrite } from "./preparation-progress";

/** Logs are fetched only when opened. Current progress lives on the deployment. */
export const loadDeploymentEvents = Effect.fn("Deployments.events")(function* (input: {
  organizationId: string; deploymentId: string; after: number;
}) {
  const { drizzle } = yield* Database;
  const [deployment] = yield* drizzle.select({ id: environmentDeployment.id, finishedAt: environmentDeployment.finishedAt })
    .from(environmentDeployment).where(and(eq(environmentDeployment.id, input.deploymentId), eq(environmentDeployment.organizationId, input.organizationId))).limit(1);
  if (!deployment) return yield* new NotFound({ message: "Deployment was not found." });
  const events = yield* drizzle.select().from(environmentDeploymentEvent)
    .where(and(eq(environmentDeploymentEvent.deploymentId, input.deploymentId), gt(environmentDeploymentEvent.id, input.after)))
    .orderBy(asc(environmentDeploymentEvent.id)).limit(50);
  const last = events.at(-1);
  return { events, finished: deployment.finishedAt !== null, nextSequence: events.length === 50 && last ? String(last.id) : null };
});

/** Steps in start order, then output after the cursor. Both are fetched only when opened. */
export const loadDeploymentBuildLog = Effect.fn("Deployments.buildLog")(function* (input: {
  organizationId: string; deploymentId: string; after: number; limit: number;
}) {
  const { drizzle } = yield* Database;
  const [deployment] = yield* drizzle.select({ id: environmentDeployment.id, finishedAt: environmentDeployment.finishedAt })
    .from(environmentDeployment).where(and(eq(environmentDeployment.id, input.deploymentId), eq(environmentDeployment.organizationId, input.organizationId))).limit(1);
  if (!deployment) return yield* new NotFound({ message: "Deployment was not found." });
  const steps = yield* drizzle.select().from(environmentDeploymentBuildStep)
    .where(eq(environmentDeploymentBuildStep.deploymentId, input.deploymentId))
    .orderBy(sql`${environmentDeploymentBuildStep.startedAt} nulls last`, asc(environmentDeploymentBuildStep.id));
  const output = yield* drizzle.select().from(environmentDeploymentBuildOutput)
    .where(and(eq(environmentDeploymentBuildOutput.deploymentId, input.deploymentId), gt(environmentDeploymentBuildOutput.id, input.after)))
    .orderBy(asc(environmentDeploymentBuildOutput.id)).limit(input.limit);
  const last = output.at(-1);
  return { steps, output, finished: deployment.finishedAt !== null, nextSequence: output.length === input.limit && last ? String(last.id) : null };
});

/** Upsert steps by key and append output. Output may precede its step; a placeholder holds its place. */
export const persistBuildLog = Effect.fn("Deployments.persistBuildLog")(function* (deploymentId: string, writes: { steps: readonly BuildStepWrite[]; output: readonly BuildOutputWrite[] }) {
  if (!writes.steps.length && !writes.output.length) return;
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    for (const step of writes.steps) {
      const { key, ...state } = step;
      yield* drizzle.insert(environmentDeploymentBuildStep).values({ deploymentId, key, ...state })
        .onConflictDoUpdate({ target: [environmentDeploymentBuildStep.deploymentId, environmentDeploymentBuildStep.key], set: { ...state, updatedAt: new Date() } });
    }
    for (const row of writes.output) {
      const [step] = yield* drizzle.insert(environmentDeploymentBuildStep).values({ deploymentId, key: row.step, name: row.step })
        .onConflictDoUpdate({ target: [environmentDeploymentBuildStep.deploymentId, environmentDeploymentBuildStep.key], set: { key: row.step } })
        .returning({ id: environmentDeploymentBuildStep.id });
      if (!step) continue;
      yield* drizzle.insert(environmentDeploymentBuildOutput).values({ deploymentId, stepId: step.id, stderr: row.stderr, text: row.text });
    }
  }));
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
