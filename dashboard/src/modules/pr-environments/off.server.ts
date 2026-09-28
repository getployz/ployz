import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { environment } from "#/modules/project/tables";
import { lockBranchScope } from "#/modules/environment-design/workspace-repository.server";
import { loadLatestEnvironmentSavedState } from "#/modules/environment-design/saved-state-repository.server";
import { admitEnvironmentDeployment } from "#/modules/deployments/admission.server";
import { cancelActiveDeployments } from "#/modules/deployments/deployment-command.server";
import { lockEnvironmentDeploymentQueue } from "#/modules/deployments/queue-lock.server";
import { dispatchEnvironmentDeployment } from "#/modules/deployments/runtime-lifecycle.repository.server";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { activeTeardownFor } from "#/modules/runtime/teardown.repository";
import { prepareShutdown } from "#/modules/runtime/teardown.server";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";
import { canShutDown, type OffCommand } from "./off";
import { prEnvironment } from "./tables";

/** The PR Environment in the actor's organization, with its name. */
const prEnvironmentOf = Effect.fn("PrEnvironments.prEnvironmentOf")(function* (actor: Actor, input: OffCommand) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ projectId: environment.projectId, name: environment.name }).from(prEnvironment)
    .innerJoin(environment, eq(environment.id, prEnvironment.environmentId))
    .where(and(eq(prEnvironment.environmentId, input.environmentId), eq(prEnvironment.organizationId, organization.id)));
  if (!row) return yield* new NotFound({ message: "The PR environment was not found." });
  return { ...row, organizationId: organization.id };
});

/**
 * Shuts a PR Environment down. Under its Branch row and deployment queue, its active attempts are cancelled and a
 * shutdown admitted, which removes its services and their data from the servers and keeps every row, its standing saves
 * included. The shutdown runs (`running`) until it ends Off, or `failed`, when Shut down can run again. Running or Off,
 * it does nothing.
 */
export const shutDownPrEnvironment = Effect.fn("PrEnvironments.shutDown")(function* (actor: Actor, input: OffCommand) {
  const { projectId, name, organizationId } = yield* prEnvironmentOf(actor, input);
  const prepared = yield* prepareShutdown({ organizationId, environmentId: input.environmentId });
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    if (!(yield* lockBranchScope(projectId, input.environmentId, "update"))) return yield* new NotFound({ message: "The PR environment was not found." });
    yield* lockEnvironmentDeploymentQueue(input.environmentId);
    const [row] = yield* drizzle.select({ shutdown: prEnvironment.shutdown }).from(prEnvironment).where(eq(prEnvironment.environmentId, input.environmentId));
    if (!canShutDown(row?.shutdown ?? null)) return;
    if ((yield* activeTeardownFor([input.environmentId])).size > 0) return yield* new Conflict({ message: `${name} is being removed.`, userFacing: true });
    yield* cancelActiveDeployments([input.environmentId]);
    yield* prepared.admit(actor.userId);
    // Last, as lock order puts a pr_environment row (lockProjectDefault).
    yield* drizzle.update(prEnvironment).set({ shutdown: "running" }).where(eq(prEnvironment.environmentId, input.environmentId));
  }));
}, Effect.scoped);

/** Deploy on an Off PR Environment: its latest Saved State again, which admission turns back on. */
export const startPrEnvironment = Effect.fn("PrEnvironments.start")(function* (actor: Actor, input: OffCommand) {
  yield* prEnvironmentOf(actor, input);
  const database = yield* Database;
  const deployment = yield* database.transaction(Effect.gen(function* () {
    const saved = yield* loadLatestEnvironmentSavedState(input.environmentId);
    if (!saved) return yield* new Conflict({ message: "Nothing is saved here to deploy.", userFacing: true });
    return yield* admitEnvironmentDeployment({
      environmentId: input.environmentId, savedStateSnapshotId: saved.id, message: null,
      triggerOrigin: { origin: "manual", actorId: actor.userId }, serviceActionPolicy: { kind: "all_affected_required" },
    });
  }));
  yield* dispatchEnvironmentDeployment({ environmentDeploymentId: deployment.id, environmentId: input.environmentId })
    .pipe(Effect.catchTag("InngestEventSendError", () => Effect.void));
  return { deploymentId: deployment.id };
});
