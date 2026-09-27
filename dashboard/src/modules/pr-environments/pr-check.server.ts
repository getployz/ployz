import "@tanstack/react-start/server-only";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { branchChanges } from "@ployz/sdk/config";
import { variableName } from "#/modules/branches/branch-review";
import { loadClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import { managedHostname } from "#/modules/environment-design/managed-service-exports";
import { fetchInstallationPullRequest, postInstallationCheckRun } from "#/modules/github/github-observation.api";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { environment, environmentBranch, project } from "#/modules/project/tables";
import { activeTeardownFor } from "#/modules/runtime/teardown.repository";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { goesToComparison } from "./conditional-save.server";
import { standing } from "./conditional-save";
import { PR_CHECK_NAME, prCheck } from "./pr-check";
import { prDestinations } from "./pr-environment.repository.server";
import { conditionalSave, prEnvironmentPlan } from "./tables";

/**
 * Posts the PR Environment's "Ployz · ready to merge" check on its pull request's head commit, computed from the state
 * as it is now, so a later post always carries the latest state. Nothing is posted for a closed pull request, a PR
 * Environment being torn down, or an installation that hasn't granted Checks: write.
 */
export const postPrCheck = Effect.fn("PrEnvironments.postPrCheck")(function* (prEnvironmentId: string) {
  const { drizzle } = yield* Database;
  const config = yield* AppConfig;
  const [pr] = yield* drizzle.select({
    branch: environmentBranch, namespace: environment.namespace, intent: environment.intent, revision: environment.revision,
    organizationId: organization.id, organizationSlug: organization.slug, projectSlug: project.slug,
    installationId: prEnvironmentPlan.installationId,
  }).from(environmentBranch)
    .innerJoin(environment, eq(environment.id, environmentBranch.environmentId))
    .innerJoin(project, eq(project.id, environmentBranch.projectId))
    .innerJoin(organization, eq(organization.id, environmentBranch.organizationId))
    .innerJoin(prEnvironmentPlan, and(eq(prEnvironmentPlan.projectId, environmentBranch.projectId), eq(prEnvironmentPlan.repositoryId, environmentBranch.prRepositoryId)))
    .where(eq(environmentBranch.environmentId, prEnvironmentId));
  if (!pr || pr.branch.prNumber === null || pr.branch.prRepositoryId === null) return "skipped" as const;
  if ((yield* activeTeardownFor([prEnvironmentId])).has(prEnvironmentId)) return "skipped" as const;
  const live = yield* fetchInstallationPullRequest(pr.installationId, pr.branch.prRepositoryId, pr.branch.prNumber);
  if (!live.open) return "skipped" as const;

  const destinationIds = yield* prDestinations(prEnvironmentId);
  const names = destinationIds.length ? yield* drizzle.select({ id: environment.id, name: environment.name }).from(environment)
    .where(inArray(environment.id, destinationIds)) : [];
  const saves = yield* drizzle.select({ save: conditionalSave, approvedBy: user.name }).from(conditionalSave)
    .leftJoin(user, eq(user.id, conditionalSave.approvedByUserId))
    .where(eq(conditionalSave.prEnvironmentId, prEnvironmentId));
  const destinations = yield* Effect.forEach(destinationIds, (destinationEnvironmentId) => Effect.gen(function* () {
    const { compare } = yield* goesToComparison({
      projectSlug: pr.projectSlug, prEnvironment: { id: prEnvironmentId, namespace: pr.namespace }, branch: pr.branch, destinationEnvironmentId,
    });
    const held = saves.find(({ save }) => save.destinationEnvironmentId === destinationEnvironmentId);
    return {
      name: names.find((row) => row.id === destinationEnvironmentId)?.name ?? "",
      changes: branchChanges(compare).rows.filter((row) => row.role === "move").length,
      approval: held ? {
        standing: standing(held.save, { id: prEnvironmentId, revision: pr.revision, targetBranch: pr.branch.prTargetBranch }),
        changes: held.save.rows.length,
        missing: held.save.rows.filter((row) => row.missing).map(({ row }) => variableName(row)),
        approvedBy: held.approvedBy,
      } : null,
    };
  }));
  const check = prCheck(destinations);

  const clusterDomain = (yield* loadClusterDomain(pr.organizationId))?.name ?? null;
  const addresses = pr.intent.services.flatMap(({ config: service }) => [
    ...service.routes.map((route) => route.hostname),
    ...(clusterDomain ? service.managedHostnames.map(({ prefix }) => managedHostname(prefix, clusterDomain)) : []),
  ]);
  return yield* postInstallationCheckRun(pr.installationId, pr.branch.prRepositoryId, {
    headSha: live.headSha,
    name: PR_CHECK_NAME,
    conclusion: check.passing ? "success" : "action_required",
    detailsUrl: new URL(`cloud/${pr.organizationSlug}/${pr.projectSlug}/${pr.namespace}/review`, config.app.url).href,
    title: check.reason,
    summary: addresses.length ? `${pr.namespace} is at:\n\n${addresses.map((address) => `- https://${address}`).join("\n")}` : `${pr.namespace} has no web addresses.`,
  }).pipe(
    Effect.as("posted" as const),
    // No Checks: write yet: the PR environments settings say so, and nothing else waits on the check.
    Effect.catchIf((error) => error.status === 403 && !error.retriable, (error) =>
      Effect.logWarning("The PR check was not posted: the installation lacks Checks: write.", { prEnvironmentId, error }).pipe(Effect.as("forbidden" as const))),
  );
});
