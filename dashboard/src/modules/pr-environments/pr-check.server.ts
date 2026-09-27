import "@tanstack/react-start/server-only";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { branchChanges } from "@ployz/sdk/config";
import { core } from "#/modules/branches/branch-operations.server";
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
import { checkDestination, PR_CHECK_NAME, prCheck } from "./pr-check";
import { prDestinations } from "./pr-environment.repository.server";
import { conditionalSave, prEnvironment, prEnvironmentPlan } from "./tables";

/**
 * Posts a pull request's "Ployz · ready to merge" check on its head commit, computed from the state as it is now, so a
 * later post always carries the latest state. Every project's PR Environment for the pull request counts towards the
 * one check. Nothing is posted for a closed pull request, one whose PR Environments are all being torn down, or an
 * installation that hasn't granted Checks: write.
 */
export const postPrCheck = Effect.fn("PrEnvironments.postPrCheck")(function* (pullRequest: { repositoryId: number; number: number }) {
  const { drizzle } = yield* Database;
  const config = yield* AppConfig;
  const rows = yield* drizzle.select({
    pr: prEnvironment, branch: environmentBranch, namespace: environment.namespace, intent: environment.intent, revision: environment.revision,
    organizationId: organization.id, organizationSlug: organization.slug, projectSlug: project.slug,
    installationId: prEnvironmentPlan.installationId,
  }).from(prEnvironment)
    .innerJoin(environmentBranch, eq(environmentBranch.environmentId, prEnvironment.environmentId))
    .innerJoin(environment, eq(environment.id, prEnvironment.environmentId))
    .innerJoin(project, eq(project.id, prEnvironment.projectId))
    .innerJoin(organization, eq(organization.id, prEnvironment.organizationId))
    .innerJoin(prEnvironmentPlan, and(eq(prEnvironmentPlan.projectId, prEnvironment.projectId), eq(prEnvironmentPlan.repositoryId, prEnvironment.repositoryId)))
    .where(and(eq(prEnvironment.repositoryId, pullRequest.repositoryId), eq(prEnvironment.number, pullRequest.number), eq(prEnvironment.closed, false)))
    .orderBy(prEnvironment.environmentId);
  const closing = yield* activeTeardownFor(rows.map((row) => row.pr.environmentId));
  const prs = rows.filter((row) => !closing.has(row.pr.environmentId));
  const [first] = prs;
  if (!first) return "skipped" as const;
  const live = yield* fetchInstallationPullRequest(first.installationId, pullRequest.repositoryId, pullRequest.number);
  if (!live.open) return "skipped" as const;

  const destinations = yield* Effect.forEach(prs, (pr) => Effect.gen(function* () {
    const prEnvironmentId = pr.pr.environmentId;
    const destinationIds = yield* prDestinations(prEnvironmentId);
    const names = destinationIds.length ? yield* drizzle.select({ id: environment.id, name: environment.name }).from(environment)
      .where(inArray(environment.id, destinationIds)) : [];
    const saves = yield* drizzle.select({ save: conditionalSave, approvedBy: user.name }).from(conditionalSave)
      .leftJoin(user, eq(user.id, conditionalSave.approvedByUserId))
      .where(eq(conditionalSave.prEnvironmentId, prEnvironmentId));
    return yield* Effect.forEach(destinationIds, (destinationEnvironmentId) => Effect.gen(function* () {
      const { compare } = yield* goesToComparison({
        projectSlug: pr.projectSlug, prEnvironment: { id: prEnvironmentId, namespace: pr.namespace }, branch: pr.branch, destinationEnvironmentId,
      });
      const held = saves.find(({ save }) => save.destinationEnvironmentId === destinationEnvironmentId);
      const { rows: changes } = yield* core("review", () => branchChanges(compare));
      return checkDestination(
        names.find((row) => row.id === destinationEnvironmentId)?.name ?? "",
        changes.filter((row) => row.role === "move").length,
        held ? {
          standing: standing(held.save, { id: prEnvironmentId, revision: pr.revision, targetBranch: pr.pr.targetBranch }),
          rows: held.save.rows, approvedBy: held.approvedBy,
        } : null,
      );
    }));
  }));
  const check = prCheck(destinations.flat());

  const summary = yield* Effect.forEach(prs, (pr) => Effect.gen(function* () {
    const clusterDomain = (yield* loadClusterDomain(pr.organizationId))?.name ?? null;
    const addresses = pr.intent.services.flatMap(({ config: service }) => [
      ...service.routes.map((route) => route.hostname),
      ...(clusterDomain ? service.managedHostnames.map(({ prefix }) => managedHostname(prefix, clusterDomain)) : []),
    ]);
    return addresses.length ? `${pr.namespace} is at:\n\n${addresses.map((address) => `- https://${address}`).join("\n")}` : `${pr.namespace} has no web addresses.`;
  }));
  return yield* postInstallationCheckRun(first.installationId, pullRequest.repositoryId, {
    headSha: live.headSha,
    name: PR_CHECK_NAME,
    conclusion: check.passing ? "success" : "action_required",
    detailsUrl: new URL(`cloud/${first.organizationSlug}/${first.projectSlug}/${first.namespace}/review`, config.app.url).href,
    title: check.reason,
    summary: summary.join("\n\n"),
  }).pipe(
    Effect.as("posted" as const),
    // No Checks: write yet: the PR environments settings say so, and nothing else waits on the check.
    Effect.catchIf((error) => error.status === 403 && !error.retriable, (error) =>
      Effect.logWarning("The PR check was not posted: the installation lacks Checks: write.", { pullRequest, error }).pipe(Effect.as("forbidden" as const))),
  );
});
