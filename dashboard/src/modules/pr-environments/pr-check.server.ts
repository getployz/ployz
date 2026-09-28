import "@tanstack/react-start/server-only";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { branchChanges } from "@ployz/sdk/config";
import { core } from "#/modules/branches/branch-operations.server";
import { loadClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import { managedHostname } from "#/modules/environment-design/managed-service-exports";
import { fetchInstallationPullRequest, postInstallationCheckRun } from "#/modules/github/github-observation.api";
import { organization } from "#/modules/organization/tables";
import { environment, environmentBranch, project } from "#/modules/project/tables";
import { activeTeardownFor } from "#/modules/runtime/teardown.repository";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { saveComparison } from "./conditional-save.server";
import { standing } from "./conditional-save";
import { PR_CHECK_NAME, prCheck } from "./pr-check";
import { prDestinations } from "./pr-environment.repository.server";
import { conditionalSave, prEnvironment, prEnvironmentPlan } from "./tables";

type PrRow = Effect.Success<ReturnType<typeof loadPrEnvironments>>[number];

/** Every project's current PR Environment for the pull request, with what the check reads of each. */
const loadPrEnvironments = Effect.fn("PrEnvironments.loadPrEnvironments")(function* (pullRequest: { repositoryId: number; number: number }) {
  const { drizzle } = yield* Database;
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
    .where(and(eq(prEnvironment.repositoryId, pullRequest.repositoryId), eq(prEnvironment.number, pullRequest.number), eq(prEnvironment.closed, false), eq(prEnvironment.retired, false)))
    .orderBy(prEnvironment.environmentId);
  const closing = yield* activeTeardownFor(rows.map((row) => row.pr.environmentId));
  return rows.filter((row) => !closing.has(row.pr.environmentId));
});

/** What one PR Environment moves into each of its Destinations, and its Conditional Save there, as the check reads them. */
const checkDestinationsOf = Effect.fn("PrEnvironments.checkDestinationsOf")(function* (pr: PrRow) {
  const { drizzle } = yield* Database;
  const prEnvironmentId = pr.pr.environmentId;
  const destinationIds = yield* prDestinations(prEnvironmentId);
  const names = destinationIds.length ? yield* drizzle.select({ id: environment.id, name: environment.name }).from(environment)
    .where(inArray(environment.id, destinationIds)) : [];
  const saves = yield* drizzle.select().from(conditionalSave).where(eq(conditionalSave.prEnvironmentId, prEnvironmentId));
  return yield* Effect.forEach(destinationIds, (destinationEnvironmentId) => Effect.gen(function* () {
    const { compare } = yield* saveComparison({
      projectSlug: pr.projectSlug, prEnvironment: { id: prEnvironmentId, namespace: pr.namespace }, branch: pr.branch, destinationEnvironmentId,
    });
    const save = saves.find((candidate) => candidate.destinationEnvironmentId === destinationEnvironmentId);
    const { rows: changes } = yield* core("review", () => branchChanges(compare));
    return {
      name: names.find((row) => row.id === destinationEnvironmentId)?.name ?? "",
      changes: changes.filter((row) => row.role === "move").length,
      save: save ? {
        standing: standing(save, { id: prEnvironmentId, revision: pr.revision, targetBranch: pr.pr.targetBranch }), changes: save.rows.length,
      } : null,
    };
  }));
});

/** Where one PR Environment is on the web, for the check's summary. */
const addressSummaryOf = Effect.fn("PrEnvironments.addressSummaryOf")(function* (pr: PrRow) {
  const clusterDomain = (yield* loadClusterDomain(pr.organizationId))?.name ?? null;
  const addresses = pr.intent.services.flatMap(({ config: service }) => [
    ...service.routes.map((route) => route.hostname),
    ...(clusterDomain ? service.managedHostnames.map(({ prefix }) => managedHostname(prefix, clusterDomain)) : []),
  ]);
  return addresses.length ? `${pr.namespace} is at:\n\n${addresses.map((address) => `- https://${address}`).join("\n")}` : `${pr.namespace} has no web addresses.`;
});

/**
 * Posts a pull request's "Ployz · ready to merge" check on its head commit, computed from the state as it is now, so a
 * later post always carries the latest state. Every project's PR Environment for the pull request counts towards the
 * one check, which GitHub knows by the pull request (its external id), so two pull requests from one head keep theirs.
 * Nothing is posted for a closed pull request, one whose PR Environments are all being torn down, or an installation
 * that hasn't granted Checks: write.
 */
export const postPrCheck = Effect.fn("PrEnvironments.postPrCheck")(function* (pullRequest: { repositoryId: number; number: number }) {
  const config = yield* AppConfig;
  const prs = yield* loadPrEnvironments(pullRequest);
  const [first] = prs;
  if (!first) return "skipped" as const;
  const live = yield* fetchInstallationPullRequest(first.installationId, pullRequest.repositoryId, pullRequest.number);
  if (!live.open) return "skipped" as const;
  const check = prCheck((yield* Effect.forEach(prs, checkDestinationsOf)).flat(), first.pr.targetBranch);
  const summary = yield* Effect.forEach(prs, addressSummaryOf);
  return yield* postInstallationCheckRun(first.installationId, pullRequest.repositoryId, {
    headSha: live.headSha,
    name: PR_CHECK_NAME,
    externalId: `${pullRequest.repositoryId}:${pullRequest.number}`,
    conclusion: check.passing ? "success" : "action_required",
    detailsUrl: new URL(`cloud/${first.organizationSlug}/${first.projectSlug}/${first.namespace}`, config.app.url).href,
    title: check.reason,
    summary: summary.join("\n\n"),
  }).pipe(
    Effect.as("posted" as const),
    // No Checks: write yet: the PR environments settings say so, and nothing else waits on the check.
    Effect.catchIf((error) => error.status === 403 && !error.retriable, (error) =>
      Effect.logWarning("The PR check was not posted: the installation lacks Checks: write.", { pullRequest, error }).pipe(Effect.as("forbidden" as const))),
  );
});
