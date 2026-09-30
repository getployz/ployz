import "@tanstack/react-start/server-only";
import type { ConfigWritten, EnvironmentSummary, PullRequestView, SystemEvent } from "@ployz/sdk";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { gatherTrusted, organizationServers } from "#/modules/config-store/config-store.server";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { unclaimedStoreDeployments } from "#/modules/config-store/store-deployment.server";
import type { StoreCall, StoreRead } from "./store.contract";
import { StoreGithubFailure, admitted, descendsFrom, pullRequestEvent } from "#/modules/config-store/store-github.server";
import { fetchInstallationPullRequest, postInstallationCheckRun, resolveGithubRepository } from "#/modules/github/github-observation.api";
import { githubRepositoryCache } from "#/modules/github/tables";
import { member } from "#/modules/identity/tables";
import type { GithubPullRequestReceivedEventData } from "#/modules/github/github-ingestion.contracts";
import { listGithubInstallationOrganizationIds } from "#/modules/github/github.repository";
import type { ConfigDeploymentAdmittedEventData, ConfigPrCheckRequestedEventData } from "#/modules/inngest/events";
import { organization } from "#/modules/organization/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

/** The check Ployz posts on a PR Environment's pull request. It never blocks a deploy; GitHub may require it to merge. */
const PR_CHECK_NAME = "Ployz · ready to merge";

/** What an observation left Cloud to do: runs to dispatch, Branches to take off the Servers, a check to publish. */
export type StoreOutcome = {
  deployments: ConfigDeploymentAdmittedEventData[];
  closing: { organizationId: string; environment: EnvironmentSummary }[];
  check: boolean;
};

/** Gather what `written` left Cloud to do into `into`; an Environment the Store skipped is logged. */
const collect = Effect.fn("StorePullRequest.collect")(function* (organizationId: string, written: ConfigWritten, into: StoreOutcome) {
  if (written.written !== "automated") return;
  into.deployments.push(...admitted(organizationId, written));
  into.closing.push(...written.closing.map((environment) => ({ organizationId, environment })));
  into.check ||= written.checks.length > 0;
  for (const skipped of written.skipped) {
    yield* Effect.logWarning("The Config Store skipped an Environment.", { organizationId, ...skipped });
  }
});

/**
 * A pull request, as each Organization of the installation's members sees it: Cloud reads its facts from GitHub now,
 * never the webhook's, and the Store keeps the newest by GitHub's `updated_at`, so a late delivery can't reopen a
 * closed one. The Store makes, retracks and closes PR Environments.
 */
export const observeStorePullRequest = Effect.fn("StorePullRequest.observe")(function* (payload: GithubPullRequestReceivedEventData) {
  const store = yield* cloudStore;
  const done: StoreOutcome = { deployments: [], closing: [], check: false };
  const organizations = yield* listGithubInstallationOrganizationIds(payload.installationId);
  if (organizations.length === 0) return done;
  const live = yield* fetchInstallationPullRequest(payload.installationId, payload.repositoryId, payload.number);
  const { updatedAt } = live;
  if (updatedAt === null) return yield* new StoreGithubFailure({ message: "GitHub sent a pull request without updated_at." });
  const merge = live.open ? null : live.mergeCommitSha;
  const repository = merge === null ? null : yield* resolveGithubRepository(payload.installationId, payload.repositoryId);
  for (const organizationId of organizations) {
    // Merged: whether the head the Store last saw of the target branch already has the merge commit, so its
    // Conditional Saves land with what that push deployed.
    let reached: string | null = null;
    if (merge !== null && repository !== null) {
      const head = yield* storeTry(() => store.branchHead(organizationId, payload.repositoryId, live.targetBranch));
      if (head !== null && (yield* descendsFrom(payload.installationId, repository, merge, head))) reached = head;
    }
    const event: SystemEvent = pullRequestEvent(payload.repositoryId, payload.number, { ...live, updatedAt }, reached);
    const servers = yield* organizationServers(organizationId);
    yield* collect(organizationId, yield* storeTry(() => store.system(organizationId, event, { servers })), done);
  }
  return done;
});

/**
 * Hourly: queued Deployments whose hand-off to the worker was lost go to it again, and each Organization's Store
 * closes Branches idle for a week, and deletes closing ones whose removal applied.
 * ponytail: every Organization each hour; list only those with Branches once that costs.
 */
export const sweepStores = Effect.fn("StorePullRequest.sweep")(function* (now: Date) {
  const store = yield* cloudStore;
  const { drizzle } = yield* Database;
  const organizations = yield* drizzle.select({ id: organization.id }).from(organization);
  const done: StoreOutcome = { deployments: [], closing: [], check: false };
  const event: SystemEvent = { event: "sweep", now: Math.floor(now.getTime() / 1000) };
  // Before the Stores admit anything new, so only admissions whose hand-off was lost are handed over again.
  done.deployments.push(...yield* unclaimedStoreDeployments(now));
  for (const { id } of organizations) {
    yield* organizationServers(id).pipe(
      Effect.flatMap((servers) => storeTry(() => store.system(id, event, { servers }))),
      Effect.flatMap((written) => collect(id, written, done)),
      // One Organization's Store failing never holds back the others; the next sweep retries it.
      Effect.catch((error) => Effect.logWarning("A Config Store sweep failed.", { organizationId: id, error })),
    );
  }
  return done;
});

/**
 * Take each Branch the Store is closing off the Servers: its removal Deployment accepts every Volume loss the Servers
 * report, since the system closes it. Resolves to the removals to dispatch. One that can't be admitted now (a
 * Deployment still running, Servers not answering) waits for the next sweep.
 */
export const closeStoreEnvironments = Effect.fn("StorePullRequest.close")(function* (closing: StoreOutcome["closing"]) {
  const store = yield* cloudStore;
  const removals: ConfigDeploymentAdmittedEventData[] = [];
  for (const { organizationId, environment: summary } of closing) {
    const read: StoreRead = (query) => store.read(organizationId, query);
    const environment = { project: summary.project, environment: summary.name };
    const written = yield* Effect.gen(function* () {
      const losses = yield* storeTry(() => read({ query: "removals", environment, remove: true }));
      const call = {
        operation: "write", command: {
          command: "admit", admit: "remove", id: crypto.randomUUID(), environment, version: null,
          accept_volume_loss: losses.volumes.map((volume) => volume.name),
        },
      } satisfies StoreCall;
      const trusted = yield* gatherTrusted(organizationId, call, read);
      return yield* storeTry(() => store.write(organizationId, call.command, trusted));
    }).pipe(Effect.option);
    if (written._tag === "Some" && written.value.written === "deployment") {
      removals.push({ organizationId, environmentId: summary.id, deploymentId: written.value.id });
    } else {
      yield* Effect.logWarning("A closing Branch's removal was not admitted; the next sweep retries it.", { organizationId, environment });
    }
  }
  return removals;
});

/**
 * A Store write named the pull request (a Conditional Save, for one): publish its check again, through the GitHub
 * installation a member of the Organization reads the repository with.
 */
export const publishRequestedStorePrCheck = Effect.fn("StorePullRequest.publishRequestedCheck")(function* (
  request: ConfigPrCheckRequestedEventData,
) {
  const { drizzle } = yield* Database;
  const [found] = yield* drizzle.select({ installationId: githubRepositoryCache.installationId }).from(githubRepositoryCache)
    .innerJoin(member, eq(member.userId, githubRepositoryCache.userId))
    .where(and(eq(member.organizationId, request.organizationId), eq(githubRepositoryCache.repositoryId, request.repositoryId)))
    .limit(1);
  if (!found) return "skipped" as const;
  return yield* publishStorePrCheck({ installationId: found.installationId, repositoryId: request.repositoryId, number: request.number });
});

/**
 * Publish the pull request's "Ployz · ready to merge" check from the Store's views, as they are now: every
 * Organization's PR Environments for it count toward the one check. Nothing is posted for a closed pull request, one
 * without PR Environments, or an installation without Checks: write.
 */
export const publishStorePrCheck = Effect.fn("StorePullRequest.publishCheck")(function* (payload: {
  installationId: number; repositoryId: number; number: number;
}) {
  const store = yield* cloudStore;
  const config = yield* AppConfig;
  const organizations = yield* listGithubInstallationOrganizationIds(payload.installationId);
  const views: { organizationId: string; view: PullRequestView }[] = [];
  for (const organizationId of organizations) {
    const view = yield* storeTry(() => store.read(organizationId, { query: "pull_request", repository_id: payload.repositoryId, number: payload.number }));
    if (view.pull_request?.open && view.environments.length > 0) views.push({ organizationId, view });
  }
  const [first] = views;
  if (!first?.view.pull_request) return "skipped" as const;
  const { drizzle } = yield* Database;
  const slugs = yield* drizzle.select({ id: organization.id, slug: organization.slug }).from(organization)
    .where(inArray(organization.id, views.map((entry) => entry.organizationId)));
  const failing = views.find((entry) => !entry.view.passing);
  const [environment] = first.view.environments;
  const slug = slugs.find((row) => row.id === first.organizationId)?.slug ?? "";
  const summary = views.flatMap(({ view }) => view.environments.map((pr) =>
    `- ${pr.environment.project}/${pr.environment.name}: ${pr.deployment ? `Deployment #${pr.deployment.number} ${pr.deployment.status}` : "not deployed"}`));
  return yield* postInstallationCheckRun(payload.installationId, payload.repositoryId, {
    headSha: first.view.pull_request.head,
    name: PR_CHECK_NAME,
    externalId: `${payload.repositoryId}:${payload.number}`,
    conclusion: failing ? "action_required" : "success",
    detailsUrl: new URL(`cloud/${slug}/${environment?.environment.project ?? ""}/${environment?.environment.name ?? ""}`, config.app.url).href,
    title: (failing ?? first).view.reason,
    summary: summary.join("\n"),
  }).pipe(
    Effect.as("posted" as const),
    Effect.catchIf((error) => error.status === 403 && !error.retriable, (error) =>
      Effect.logWarning("The PR check was not posted: the installation lacks Checks: write.", { payload, error }).pipe(Effect.as("forbidden" as const))),
  );
});
