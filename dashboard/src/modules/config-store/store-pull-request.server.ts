import "@tanstack/react-start/server-only";
import type { ConfigWritten, EnvironmentSummary, PullRequestView, SystemEvent } from "@ployz/sdk";
import { inArray } from "drizzle-orm";
import { Effect } from "effect";
import { callStore, cloudStore } from "#/modules/config-store/config-store.server";
import { StoreGithubFailure } from "#/modules/config-store/store-github.server";
import { fetchInstallationPullRequest, postInstallationCheckRun } from "#/modules/github/github-observation.api";
import type { GithubPullRequestReceivedEventData } from "#/modules/github/github-ingestion.contracts";
import { listGithubInstallationOrganizationIds } from "#/modules/github/github.repository";
import type { ConfigDeploymentAdmittedEventData } from "#/modules/inngest/events";
import { organization } from "#/modules/organization/tables";
import { PR_CHECK_NAME } from "#/modules/pr-environments/pr-check";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

const storeCall = <A>(call: () => Promise<A>) =>
  Effect.tryPromise({ try: call, catch: (cause) => new StoreGithubFailure({ cause }) });

/** What an observation left Cloud to do: runs to dispatch, Branches to take off the Servers, a check to publish. */
export type StoreOutcome = {
  deployments: ConfigDeploymentAdmittedEventData[];
  closing: { organizationId: string; environment: EnvironmentSummary }[];
  check: boolean;
};

/** Gather what `written` left Cloud to do into `into`; an Environment the Store skipped is logged. */
const collect = Effect.fn("StorePullRequest.collect")(function* (organizationId: string, written: ConfigWritten, into: StoreOutcome) {
  if (written.written !== "automated") return;
  into.deployments.push(...written.admitted.map((deployed) => ({
    organizationId, environmentId: deployed.environment, deploymentId: deployed.deployment.id,
  })));
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
  if (live.updatedAt === null) return yield* new StoreGithubFailure({ cause: "GitHub sent a pull request without updated_at." });
  const event: SystemEvent = {
    event: "pull_request", repository_id: payload.repositoryId, number: payload.number, title: live.title,
    author: live.author.login, bot: live.author.isBot, head_branch: live.headBranch, head: live.headSha,
    target_branch: live.targetBranch, commits: live.commits, open: live.open, merge_commit: live.mergeCommitSha,
    updated: new Date(live.updatedAt).toISOString().replace(/\.\d{3}Z$/, "Z"),
  };
  for (const organizationId of organizations) {
    yield* collect(organizationId, yield* storeCall(() => store.system(organizationId, event)), done);
  }
  return done;
});

/**
 * Hourly: each Organization's Store closes Branches idle for a week, and deletes closing ones whose removal applied.
 * ponytail: every Organization each hour; list only those with Branches once that costs.
 */
export const sweepStores = Effect.fn("StorePullRequest.sweep")(function* (now: Date) {
  const store = yield* cloudStore;
  const { drizzle } = yield* Database;
  const organizations = yield* drizzle.select({ id: organization.id }).from(organization);
  const done: StoreOutcome = { deployments: [], closing: [], check: false };
  const event: SystemEvent = { event: "sweep", now: Math.floor(now.getTime() / 1000) };
  for (const { id } of organizations) {
    yield* storeCall(() => store.system(id, event)).pipe(
      Effect.flatMap((written) => collect(id, written, done)),
      // One Organization's Store failing never holds back the others; the next sweep retries it.
      Effect.catch((error) => Effect.logWarning("A Config Store sweep failed.", { organizationId: id, error })),
    );
  }
  return done;
});

/**
 * Take each Branch the Store is closing off the Servers: its removal Deployment accepts every Volume loss the Servers
 * report, since the system closes it, and goes to Cloud's worker like any admission. One that can't be admitted now
 * (a Deployment still running, Servers not answering) waits for the next sweep.
 */
export const closeStoreEnvironments = Effect.fn("StorePullRequest.close")(function* (closing: StoreOutcome["closing"]) {
  const store = yield* cloudStore;
  let admitted = 0;
  for (const { organizationId, environment: summary } of closing) {
    const environment = { project: summary.project, environment: summary.name };
    const removals = yield* storeCall(() => store.read(organizationId, { query: "removals", environment, remove: true }));
    const accept = removals.view === "removals" ? removals.volumes.map((volume) => volume.name) : [];
    const result = yield* callStore(organizationId, "system", {
      operation: "write",
      command: {
        command: "admit", id: crypto.randomUUID(), environment, services: [], version: null, remove: true,
        accept_volume_loss: accept,
      },
    });
    if (result.ok) admitted += 1;
    else yield* Effect.logWarning("A closing Branch's removal was not admitted; the next sweep retries it.", { organizationId, environment, refusal: result.refusal });
  }
  return admitted;
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
    const view = yield* storeCall(() => store.read(organizationId, { query: "pull_request", repository_id: payload.repositoryId, number: payload.number }));
    if (view.view === "pull_request" && view.pull_request?.open && view.environments.length > 0) views.push({ organizationId, view });
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
