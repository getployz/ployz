import "@tanstack/react-start/server-only";
import type { ConfigWritten, EnvironmentSummary, PullRequestView, SystemEvent } from "@ployz/sdk";
import { and, eq, inArray } from "drizzle-orm";
import { Effect, Option } from "effect";
import { storePrCheckRun } from "./store-pull-requests";
import { admittedEvents, callStore, storeSystem } from "#/modules/config-store/config-store.server";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { StoreGithubFailure, mergedInto, pullRequestEvent } from "#/modules/config-store/store-github.server";
import { organizationAccess } from "#/modules/github/github-access";
import { fetchInstallationPullRequest, githubLacksPermission, postInstallationCheckRun } from "#/modules/github/github-observation.api";
import { githubRepositoryCache, organizationGithubInstallation } from "#/modules/github/tables";
import type { GithubPullRequestReceivedEventData } from "#/modules/github/github-ingestion.contracts";
import { listLinkedOrganizationIds } from "#/modules/github/github.repository";
import type { ConfigDeploymentAdmittedEventData, ConfigPrCheckRequestedEventData } from "#/modules/inngest/events";
import { organization } from "#/modules/organization/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

/** What an observation left Cloud to do: runs to dispatch, Environments to take off the Servers, a check to publish. */
export type StoreOutcome = {
  deployments: ConfigDeploymentAdmittedEventData[];
  /** `shutDown`: an idle PR Environment that comes off the Servers and stays; else a due removal. */
  closing: { organizationId: string; environment: EnvironmentSummary; shutDown?: true }[];
  check: boolean;
};

/** Gather what `written` left Cloud to do into `into`; an Environment the Store skipped is logged. */
const collect = Effect.fn("StorePullRequest.collect")(function* (organizationId: string, written: ConfigWritten, into: StoreOutcome) {
  if (written.written !== "automated") return;
  into.deployments.push(...admittedEvents(organizationId, written));
  into.closing.push(...written.closing.map((environment) => ({ organizationId, environment })));
  into.closing.push(...written.shutting_down.map((environment) => ({ organizationId, environment, shutDown: true as const })));
  into.check ||= written.checks.length > 0;
  for (const skipped of written.skipped) {
    yield* Effect.logWarning("The Config Store skipped an Environment.", { organizationId, ...skipped });
  }
});

/**
 * A pull request, as each Organization linked to the installation sees it: Cloud reads its facts from GitHub now,
 * never the webhook's, and the Store keeps the newest by GitHub's `updated_at`, so a late delivery can't reopen a
 * closed one. The Store makes, retracks and closes PR Environments. The read goes through the first linked
 * Organization: every linked one shares the installation's token.
 */
export const observeStorePullRequest = Effect.fn("StorePullRequest.observe")(function* (payload: GithubPullRequestReceivedEventData) {
  const done: StoreOutcome = { deployments: [], closing: [], check: false };
  const organizations = yield* listLinkedOrganizationIds(payload.installationId);
  const first = organizations[0];
  if (first === undefined) return done;
  const access = organizationAccess(first, payload.installationId);
  const live = yield* fetchInstallationPullRequest(access, payload.repositoryId, payload.number);
  const { updatedAt } = live;
  if (updatedAt === null) return yield* new StoreGithubFailure({ message: "GitHub sent a pull request without updated_at." });
  const targetHead = yield* mergedInto(access, payload.repositoryId, live);
  const event: SystemEvent = pullRequestEvent(payload.repositoryId, payload.number, { ...live, updatedAt }, targetHead);
  for (const organizationId of organizations) {
    yield* collect(organizationId, yield* storeSystem(organizationId, event), done);
  }
  return done;
});

/**
 * Hourly: each Organization's Store closes Branches idle for a week, and deletes closing ones whose removal applied.
 * ponytail: every Organization each hour; list only those with Branches once that costs.
 */
export const sweepStores = Effect.fn("StorePullRequest.sweep")(function* (now: Date) {
  const { drizzle } = yield* Database;
  const organizations = yield* drizzle.select({ id: organization.id }).from(organization);
  const done: StoreOutcome = { deployments: [], closing: [], check: false };
  for (const { id } of organizations) {
    yield* sweepStore(id, now, done).pipe(
      // One Organization's Store failing never holds back the others; the next sweep retries it.
      Effect.catch((error) => Effect.logWarning("A Config Store sweep failed.", { organizationId: id, error })),
    );
  }
  return done;
});

/** Sweep one Organization's Store now, gathering what it left Cloud to do into `done`. */
export const sweepStore = Effect.fn("StorePullRequest.sweepOne")(function* (
  organizationId: string, now: Date, done: StoreOutcome = { deployments: [], closing: [], check: false },
) {
  const event: SystemEvent = { event: "sweep", now: Math.floor(now.getTime() / 1000) };
  yield* collect(organizationId, yield* storeSystem(organizationId, event), done);
  return done;
});

/**
 * Take each Environment whose removal is due, or idle PR Environment that shuts down, off the Servers, admitted like any Deployment (and so handed to the
 * worker). Consent was given when the removal was asked for, so admitting asks for nothing more. Resolves to the
 * removals admitted. One that can't be admitted now (a Deployment still running, Servers not answering) waits for the
 * next sweep.
 */
export const closeStoreEnvironments = Effect.fn("StorePullRequest.close")(function* (closing: StoreOutcome["closing"]) {
  const removals: string[] = [];
  for (const { organizationId, environment: summary, shutDown } of closing) {
    const environment = { project: summary.project, environment: summary.name };
    const admit = shutDown ? "shut_down" as const : "remove" as const;
    const admitted = yield* callStore(organizationId, null, {
      operation: "write",
      command: { command: "admit", admit, id: crypto.randomUUID(), environment },
    }).pipe(Effect.option);
    const result = Option.getOrUndefined(admitted);
    if (result?.ok && result.value.written === "deployment") removals.push(result.value.id);
    else yield* Effect.logWarning("A due removal was not admitted; the next sweep retries it.", { organizationId, environment, result });
  }
  return removals;
});

/**
 * A Store write named the pull request (a Conditional Sync, for one): publish its check again, through a GitHub
 * installation the Organization links that holds the repository.
 */
export const publishRequestedStorePrCheck = Effect.fn("StorePullRequest.publishRequestedCheck")(function* (
  request: ConfigPrCheckRequestedEventData,
) {
  const { drizzle } = yield* Database;
  const [found] = yield* drizzle.select({ installationId: githubRepositoryCache.installationId }).from(githubRepositoryCache)
    .innerJoin(organizationGithubInstallation, and(
      eq(organizationGithubInstallation.installationId, githubRepositoryCache.installationId),
      eq(organizationGithubInstallation.organizationId, request.organizationId),
    ))
    .where(eq(githubRepositoryCache.repositoryId, request.repositoryId))
    .orderBy(githubRepositoryCache.installationId)
    .limit(1);
  if (!found) return "skipped" as const;
  return yield* publishStorePrCheck({ installationId: found.installationId, repositoryId: request.repositoryId, number: request.number });
});

/**
 * Publish the pull request's "Ployz · ready to merge" check from the Store's views, as they are now. Nothing is posted
 * when no Store has a word (no PR Environments, closed unmerged, or everything landed), or for an installation without
 * Checks: write.
 */
export const publishStorePrCheck = Effect.fn("StorePullRequest.publishCheck")(function* (payload: {
  installationId: number; repositoryId: number; number: number;
}) {
  const store = yield* cloudStore;
  const config = yield* AppConfig;
  const organizations = yield* listLinkedOrganizationIds(payload.installationId);
  const views: { organizationId: string; view: PullRequestView }[] = [];
  for (const organizationId of organizations) {
    const view = yield* storeTry(() => store.read(organizationId, { query: "pull_request", repository_id: payload.repositoryId, number: payload.number }));
    if (view.check) views.push({ organizationId, view });
  }
  if (views.length === 0) return "skipped" as const;
  const { drizzle } = yield* Database;
  const slugs = yield* drizzle.select({ id: organization.id, slug: organization.slug }).from(organization)
    .where(inArray(organization.id, views.map((entry) => entry.organizationId)));
  const run = storePrCheckRun(
    views.map(({ organizationId, view }) => ({ slug: slugs.find((row) => row.id === organizationId)?.slug ?? "", view })),
    config.app.url.href,
  );
  if (!run) return "skipped" as const;
  const first = views[0];
  if (first === undefined) return "skipped" as const;
  return yield* postInstallationCheckRun(organizationAccess(first.organizationId, payload.installationId), payload.repositoryId, {
    ...run,
    externalId: `${payload.repositoryId}:${payload.number}`,
  }).pipe(
    Effect.as("posted" as const),
    Effect.catchIf(githubLacksPermission, (error) =>
      Effect.logWarning("The PR check was not posted: the installation lacks Checks: write.", { payload, error }).pipe(Effect.as("forbidden" as const))),
  );
});
