import "@tanstack/react-start/server-only";
import { and, asc, eq, gt, inArray, sql } from "drizzle-orm";
import { Effect } from "effect";
import { organizationIdForDeployment } from "#/db/scope-values.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { environmentDeployment, environmentDeploymentBuildOutput, environmentDeploymentBuildStep, environmentDeploymentEvent, environmentDeploymentImageBuild } from "./tables";
import type { BuildOutputWrite, BuildStepWrite } from "./preparation-progress";
import type { ImageBuildEvidence } from "./deployment-view";

/** Logs are fetched only when opened. Live progress is the latest event; terminal progress lives on the deployment. */
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

/**
 * Steps in start order, then output after the cursor; `tail` instead reads only each step's last rows, for the canvas.
 * Both are fetched only when opened.
 */
export const loadDeploymentBuildLog = Effect.fn("Deployments.buildLog")(function* (input: {
  organizationId: string; deploymentId: string; after: number; limit: number; tail?: number;
}) {
  const { drizzle } = yield* Database;
  const [deployment] = yield* drizzle.select({ id: environmentDeployment.id, finishedAt: environmentDeployment.finishedAt })
    .from(environmentDeployment).where(and(eq(environmentDeployment.id, input.deploymentId), eq(environmentDeployment.organizationId, input.organizationId))).limit(1);
  if (!deployment) return yield* new NotFound({ message: "Deployment was not found." });
  const steps = yield* drizzle.select().from(environmentDeploymentBuildStep)
    .where(eq(environmentDeploymentBuildStep.deploymentId, input.deploymentId))
    .orderBy(sql`${environmentDeploymentBuildStep.startedAt} nulls last`, asc(environmentDeploymentBuildStep.id));
  const table = environmentDeploymentBuildOutput;
  const output = input.tail === undefined
    ? yield* drizzle.select().from(table)
      .where(and(eq(table.deploymentId, input.deploymentId), gt(table.id, input.after)))
      .orderBy(asc(table.id)).limit(input.limit)
    // ponytail: ranks every output row of the attempt; index (step_id, id) if logs outgrow a quick scan.
    : yield* drizzle.select().from(table).where(inArray(table.id, drizzle.select({ id: sql<number>`ranked.id` }).from(
      drizzle.select({ id: table.id, rank: sql<number>`row_number() over (partition by ${table.stepId} order by ${table.id} desc)`.as("rank") })
        .from(table).where(eq(table.deploymentId, input.deploymentId)).as("ranked"),
    ).where(sql`ranked.rank <= ${input.tail}`))).orderBy(asc(table.id));
  // Which Server (or GitHub run) each image builds on and what it skipped; never the receipt, grant or run state.
  const builds = environmentDeploymentImageBuild;
  const imageBuilds: ImageBuildEvidence[] = yield* drizzle.select({
    image: builds.image, serverChoice: builds.serverChoice, skips: builds.skips,
    runUrl: sql<string | null>`${builds.github} ->> 'runUrl'`,
  }).from(builds).where(eq(builds.deploymentId, input.deploymentId)).pipe(Effect.map((rows) => rows.map(({ runUrl, ...row }) =>
    ({ ...row, github: runUrl ? { runUrl } : null }))));
  const last = output.at(-1);
  return { steps, output, imageBuilds, finished: deployment.finishedAt !== null, nextSequence: input.tail === undefined && output.length === input.limit && last ? String(last.id) : null };
});

/** Whose Build Steps a write files: one Builder's go (`attempt`) at one Image Build (`image`); null is the deploy step's own. */
export type BuildStepOwner = { image: string; attempt: number } | null;

/** Upsert steps by key and append output. Output may precede its step; a placeholder holds its place. */
export const persistBuildLog = Effect.fn("Deployments.persistBuildLog")(function* (deploymentId: string, writes: { steps: readonly BuildStepWrite[]; output: readonly BuildOutputWrite[] }, owner: BuildStepOwner = null) {
  if (!writes.steps.length && !writes.output.length) return;
  const database = yield* Database;
  const filed = { organizationId: organizationIdForDeployment(deploymentId), deploymentId, image: owner?.image ?? null, attempt: owner?.attempt ?? 0 };
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const table = environmentDeploymentBuildStep;
    const target = [table.deploymentId, table.image, table.attempt, table.build, table.key];
    const ids = new Map<string, number>();
    if (writes.steps.length) {
      const excluded = (column: { name: string }) => sql.raw(`excluded."${column.name}"`);
      const steps = new Map(writes.steps.map((step) => [`${step.build}:${step.key}`, step]));
      const upserted = yield* drizzle.insert(table).values([...steps.values()].map((step) => ({ ...filed, ...step })))
        .onConflictDoUpdate({ target, set: { name: excluded(table.name), startedAt: excluded(table.startedAt), completedAt: excluded(table.completedAt), cached: excluded(table.cached), error: excluded(table.error), updatedAt: new Date() } })
        .returning({ id: table.id, build: table.build, key: table.key });
      for (const step of upserted) ids.set(`${step.build}:${step.key}`, step.id);
    }
    if (!writes.output.length) return;
    const unknown = new Map(writes.output.filter((row) => !ids.has(`${row.build}:${row.step}`)).map((row) => [`${row.build}:${row.step}`, row]));
    if (unknown.size) {
      const found = yield* drizzle.insert(table).values([...unknown.values()].map((row) => ({ ...filed, build: row.build, key: row.step, name: row.step })))
        .onConflictDoUpdate({ target, set: { key: table.key } })
        .returning({ id: table.id, build: table.build, key: table.key });
      for (const step of found) ids.set(`${step.build}:${step.key}`, step.id);
    }
    yield* drizzle.insert(environmentDeploymentBuildOutput).values(writes.output.flatMap((row) => {
      const stepId = ids.get(`${row.build}:${row.step}`);
      return stepId === undefined ? [] : [{ organizationId: filed.organizationId, deploymentId, stepId, stderr: row.stderr, text: row.text }];
    }));
  }));
});

/** Opens a step once: a retried write leaves it as it is, even closed since. */
export const openBuildStep = Effect.fn("Deployments.openBuildStep")(function* (deploymentId: string, owner: NonNullable<BuildStepOwner>, step: BuildStepWrite) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(environmentDeploymentBuildStep)
    .values({ organizationId: organizationIdForDeployment(deploymentId), deploymentId, ...owner, ...step }).onConflictDoNothing();
});

export const persistDeploymentProgress = Effect.fn("Deployments.persistProgress")(function* (deploymentId: string, progress: typeof environmentDeploymentEvent.$inferInsert.progress) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(environmentDeploymentEvent).values({ organizationId: organizationIdForDeployment(deploymentId), deploymentId, progress });
});
