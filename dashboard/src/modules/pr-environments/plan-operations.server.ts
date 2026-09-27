import "@tanstack/react-start/server-only";
import { and, asc, eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { member } from "#/modules/identity/tables";
import { getProjectContextForActor } from "#/modules/environment-design/workspace-repository.server";
import { environment } from "#/modules/project/tables";
import { Database } from "#/server/database.server";
import { NotFound, Validation } from "#/server/public-error";
import { planRepositories } from "./repositories";
import { isPrEnvironment } from "./pr-environment.repository.server";
import type { SetPrEnvironmentPlan } from "./plan-schemas";
import { prEnvironmentPlan } from "./tables";

/**
 * Saves a project's PR Environments plan for one repository its services deploy from. The start-from Environment must be
 * one of the project's. Turning PR Environments on records the member they act as.
 */
export const setPrEnvironmentPlan = Effect.fn("PrEnvironments.setPlan")(function* (actor: Actor, input: SetPrEnvironmentPlan) {
  const context = yield* getProjectContextForActor(actor, input);
  if (context === null) return yield* new NotFound({ message: "Project not found." });
  const projectId = context.project.id;
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const environments = yield* drizzle.select({ id: environment.id, intent: environment.intent })
      .from(environment).where(eq(environment.projectId, projectId));
    const repository = planRepositories(environments).find((candidate) => candidate.repositoryId === input.repositoryId);
    if (!repository) return yield* new Validation({ message: "No service in this project deploys from that repository." });
    if (input.startFromEnvironmentId !== null && !environments.some((row) => row.id === input.startFromEnvironmentId)) {
      return yield* new Validation({ message: "Pick an environment of this project to start from." });
    }
    if (input.startFromEnvironmentId !== null && (yield* isPrEnvironment(input.startFromEnvironmentId))) {
      return yield* new Validation({ message: "A PR environment can't be where PR environments start from." });
    }
    const key = and(eq(prEnvironmentPlan.projectId, projectId), eq(prEnvironmentPlan.repositoryId, input.repositoryId));
    const [existing] = yield* drizzle.select().from(prEnvironmentPlan).where(key).for("update");
    const values = {
      installationId: repository.installationId,
      repository: repository.repository,
      enabled: input.enabled,
      startFromEnvironmentId: input.startFromEnvironmentId,
      picks: input.picks,
      setupCommands: input.setupCommands,
      removeOnClose: input.removeOnClose,
      includeBots: input.includeBots,
      enabledByUserId: !input.enabled ? null : existing?.enabled && existing.enabledByUserId ? existing.enabledByUserId : actor.userId,
    };
    const [row] = yield* drizzle.insert(prEnvironmentPlan)
      .values({ organizationId: context.organization.id, projectId, repositoryId: input.repositoryId, ...values })
      .onConflictDoUpdate({ target: [prEnvironmentPlan.projectId, prEnvironmentPlan.repositoryId], set: { ...values, updatedAt: new Date() } })
      .returning();
    if (!row) return yield* new NotFound({ message: "Project not found." });
    return row;
  }));
});

/**
 * Who a plan's PR Environments act as: the member who turned them on while they're still in the organization, else its
 * first owner, who then stands recorded instead. Null for a plan that's off.
 */
export const actingMember = Effect.fn("PrEnvironments.actingMember")(function* (plan: typeof prEnvironmentPlan.$inferSelect) {
  if (!plan.enabled) return null;
  const { drizzle } = yield* Database;
  const inOrganization = eq(member.organizationId, plan.organizationId);
  if (plan.enabledByUserId !== null) {
    const [enabler] = yield* drizzle.select({ userId: member.userId }).from(member)
      .where(and(inOrganization, eq(member.userId, plan.enabledByUserId)));
    if (enabler) return enabler.userId;
  }
  const [owner] = yield* drizzle.select({ userId: member.userId }).from(member)
    .where(and(inOrganization, eq(member.role, "owner"))).orderBy(asc(member.createdAt)).limit(1);
  if (!owner) return null;
  yield* drizzle.update(prEnvironmentPlan).set({ enabledByUserId: owner.userId })
    .where(and(eq(prEnvironmentPlan.projectId, plan.projectId), eq(prEnvironmentPlan.repositoryId, plan.repositoryId)));
  return owner.userId;
});
