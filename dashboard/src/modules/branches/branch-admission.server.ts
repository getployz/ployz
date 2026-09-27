import "@tanstack/react-start/server-only";
import { and, eq, isNull } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { environmentBranch } from "#/modules/project/tables";
import { service } from "#/modules/environment-design/tables";
import type { SavedDeploymentTarget } from "#/modules/deployments/admission.server";

/**
 * What a Branch adds to an attempt at admission, under the queue lock: each never-deployed Own Copy's Setup Commands,
 * by service id. A root Environment adds nothing.
 */
export const branchAdmission = Effect.fn("Branches.branchAdmission")(function* (environmentId: string, target: SavedDeploymentTarget) {
  const { drizzle } = yield* Database;
  const [branch] = yield* drizzle.select({ setupCommands: environmentBranch.setupCommands })
    .from(environmentBranch).where(eq(environmentBranch.environmentId, environmentId));
  const setupCommands: Record<string, string[]> = {};
  if (branch?.setupCommands.length) {
    const fresh = yield* drizzle.select({ id: service.id, lineageId: service.lineageId }).from(service)
      .where(and(eq(service.environmentId, environmentId), isNull(service.firstDeployedAt)));
    for (const { id, lineageId } of fresh) {
      const commands = branch.setupCommands.filter((setup) => setup.lineageId === lineageId).map((setup) => setup.command);
      if (commands.length) setupCommands[id] = commands;
    }
  }
  return { ...target, setupCommands };
});
