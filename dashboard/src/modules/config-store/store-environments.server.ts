import "@tanstack/react-start/server-only";
import type { ConfigWritten, EnvironmentSummary } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { emptyEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { environment, project } from "#/modules/project/tables";
import { Database } from "#/server/database.server";

// TODO(#1267): Cloud's routes still find Environments in its own `environment` table, by the namespace the Store
// names them by. Until they read the Store, a Branch the Store creates gets a Cloud row with its id, named and
// addressed by its Store name, and a removed Environment loses its row.
/**
 * Keeps Cloud's Environment rows in step with a Store write: a Branch (whoever made it, the dashboard or the CLI)
 * gets its row, so its canvas opens; a removed Environment's row goes. A Project Cloud doesn't know is skipped.
 */
export const followStoreEnvironments = Effect.fn("ConfigStore.followEnvironments")(function* (
  organizationId: string, written: ConfigWritten,
) {
  if (written.written === "branch") yield* addEnvironment(organizationId, written.branch.environment);
  if (written.written === "environment_removed") yield* removeEnvironment(organizationId, written.environment);
});

const projectOf = Effect.fn("ConfigStore.projectOf")(function* (organizationId: string, summary: EnvironmentSummary) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ id: project.id }).from(project)
    .where(and(eq(project.organizationId, organizationId), eq(project.slug, summary.project)));
  return row;
});

const addEnvironment = Effect.fn("ConfigStore.addEnvironment")(function* (organizationId: string, summary: EnvironmentSummary) {
  const found = yield* projectOf(organizationId, summary);
  if (!found) return;
  const { drizzle } = yield* Database;
  // Idempotent: every Branch write lands here; a name or namespace Cloud already has keeps its row.
  yield* drizzle.insert(environment).values({
    id: summary.id, projectId: found.id, organizationId, name: summary.name, namespace: summary.name,
    intent: emptyEnvironmentIntent(summary.name),
  }).onConflictDoNothing();
});

const removeEnvironment = Effect.fn("ConfigStore.removeEnvironment")(function* (organizationId: string, summary: EnvironmentSummary) {
  const found = yield* projectOf(organizationId, summary);
  if (!found) return;
  const { drizzle } = yield* Database;
  yield* drizzle.delete(environment).where(and(
    eq(environment.organizationId, organizationId),
    eq(environment.projectId, found.id),
    eq(environment.namespace, summary.name),
  ));
});
