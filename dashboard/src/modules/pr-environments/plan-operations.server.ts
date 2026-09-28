import "@tanstack/react-start/server-only";
import { and, asc, eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { member } from "#/modules/identity/tables";
import { getProjectContextForActor } from "#/modules/environment-design/workspace-repository.server";
import { environment } from "#/modules/project/tables";
import { Database } from "#/server/database.server";
import { NotFound, Validation } from "#/server/public-error";
import { defaultPrEnvironmentPlan, planRepositories } from "./repositories";
import { isPrEnvironment } from "./pr-environment.repository.server";
import type { SetPrEnvironmentPlan } from "./plan-schemas";
import { prEnvironmentPlan } from "./tables";

/**
 * Changes a project's PR Environments plan for one repository its services deploy from, field by field over the saved
 * one (or the default), so quick edits don't overwrite each other. The start-from Environment must be one of the
 * project's. Turning PR Environments on records the member they act as.
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
    const { organizationSlug: _organization, projectSlug: _project, repositoryId: _repository, ...change } = input;
    const startFrom = change.startFromEnvironmentId;
    if (startFrom != null && !environments.some((row) => row.id === startFrom)) {
      return yield* new Validation({ message: "Pick an environment of this project to start from." });
    }
    if (startFrom != null && (yield* isPrEnvironment(startFrom))) {
      return yield* new Validation({ message: "Pick an environment that isn't a PR environment." });
    }
    const key = and(eq(prEnvironmentPlan.projectId, projectId), eq(prEnvironmentPlan.repositoryId, input.repositoryId));
    const [existing] = yield* drizzle.select().from(prEnvironmentPlan).where(key).for("update");
    const plan = { ...defaultPrEnvironmentPlan, startFromEnvironmentId: null, ...existing, ...change };
    const values = {
      installationId: repository.installationId,
      repository: repository.repository,
      enabled: plan.enabled,
      startFromEnvironmentId: plan.startFromEnvironmentId,
      picks: plan.picks,
      setupCommands: plan.setupCommands,
      removeOnClose: plan.removeOnClose,
      includeBots: plan.includeBots,
      enabledByUserId: !plan.enabled ? null : existing?.enabled && existing.enabledByUserId ? existing.enabledByUserId : actor.userId,
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
 * first owner. Null when the organization has neither.
 */
export const actingMember = Effect.fn("PrEnvironments.actingMember")(function* (plan: typeof prEnvironmentPlan.$inferSelect) {
  const { drizzle } = yield* Database;
  const inOrganization = eq(member.organizationId, plan.organizationId);
  if (plan.enabledByUserId !== null) {
    const [enabler] = yield* drizzle.select({ userId: member.userId }).from(member)
      .where(and(inOrganization, eq(member.userId, plan.enabledByUserId)));
    if (enabler) return enabler.userId;
  }
  const [owner] = yield* drizzle.select({ userId: member.userId }).from(member)
    .where(and(inOrganization, eq(member.role, "owner"))).orderBy(asc(member.createdAt)).limit(1);
  return owner?.userId ?? null;
});
