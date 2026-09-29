import "@tanstack/react-start/server-only";
import type { ConfigStore, ConfigWritten, SystemEvent } from "@ployz/sdk";
import { Data, Effect } from "effect";
import { cloudStore } from "#/modules/config-store/config-store.server";
import type { StoreDeploymentServices } from "#/modules/config-store/store-deployment.server";
import {
  compareInstallationRepositoryCommits,
  fetchInstallationCheckSuite,
  fetchInstallationPullRequest,
  isGithubObservationNotFound,
  resolveGithubBranchHead,
  resolveGithubRepository,
  type GithubResolvedRepository,
} from "#/modules/github/github-observation.api";
import type {
  GithubCheckSuiteReceivedEventData,
  GithubPushReceivedEventData,
} from "#/modules/github/github-ingestion.contracts";
import { listGithubInstallationOrganizationIds } from "#/modules/github/github.repository";
import type { ConfigDeploymentAdmittedEventData } from "#/modules/inngest/events";
import { storeTry } from "#/modules/config-store/store-sdk.server";

/** What Cloud's GitHub workers need to feed the Store. */
export type StoreGithubServices = StoreDeploymentServices;

export class StoreGithubFailure extends Data.TaggedError("StoreGithubFailure")<{ readonly cause: unknown }> {}

const storeCall = <A>(call: () => Promise<A>) =>
  storeTry(call).pipe(Effect.mapError((cause) => new StoreGithubFailure({ cause })));

function isConflict(cause: unknown): cause is StoreGithubFailure {
  return cause instanceof StoreGithubFailure && typeof cause.cause === "object" && cause.cause !== null
    && "code" in cause.cause && cause.cause.code === "conflict";
}

/** GitHub's `updated_at` as the Store orders facts by it. */
export const githubTimestamp = (at: string) => new Date(at).toISOString().replace(/\.\d{3}Z$/, "Z");

type LivePullRequest = Effect.Success<ReturnType<typeof fetchInstallationPullRequest>>;

/**
 * A pull request's facts as the Store takes them. `mergeReached`: once merged, the target branch head the Store last
 * saw, when Cloud found the merge commit in it.
 */
export function pullRequestEvent(
  repositoryId: number, number: number, live: LivePullRequest & { updatedAt: string }, mergeReached: string | null,
): SystemEvent {
  return {
    event: "pull_request", repository_id: repositoryId, number, title: live.title,
    author: live.author.login, bot: live.author.isBot, head_branch: live.headBranch, head: live.headSha,
    target_branch: live.targetBranch, commits: live.commits, open: live.open, merge_commit: live.mergeCommitSha,
    merge_reached: mergeReached, updated: githubTimestamp(live.updatedAt),
  };
}

/** Whether `sha` is, or descends from, `ancestor`. */
export const descendsFrom = (installationId: number, repository: GithubResolvedRepository, ancestor: string, sha: string) =>
  ancestor === sha
    ? Effect.succeed(true)
    : compareInstallationRepositoryCommits(installationId, repository, ancestor, sha).pipe(
      Effect.map(({ status }) => status === "ahead" || status === "identical"),
      Effect.catchIf(isGithubObservationNotFound, () => Effect.succeed(false)),
    );

/**
 * Pull requests into the pushed branch that merged before their closed delivery arrived: the Store hears it now, so
 * their Conditional Saves freeze and a push with the merge commit carries them. Asks GitHub only when one stands.
 * ponytail: a PR Environment this closes leaves the Servers at the next sweep.
 */
const freezeMerged = Effect.fn("StoreGithub.freezeMerged")(function* (
  store: ConfigStore, organizationId: string, payload: GithubPushReceivedEventData,
) {
  const { standing } = yield* storeCall(() => store.pendingSaves(organizationId, payload.repositoryId, payload.branch));
  for (const number of standing) {
    const live = yield* fetchInstallationPullRequest(payload.installationId, payload.repositoryId, number).pipe(
      Effect.catchIf(isGithubObservationNotFound, () => Effect.succeed(null)),
    );
    const { updatedAt } = live ?? {};
    if (!live?.mergeCommitSha || live.targetBranch !== payload.branch || !updatedAt) continue;
    yield* storeCall(() => store.system(organizationId, pullRequestEvent(payload.repositoryId, number, { ...live, updatedAt }, null)));
  }
});

/** The merge commits of frozen Conditional Saves on the branch that `head` contains: the push carries those. */
const mergedInto = Effect.fn("StoreGithub.mergedInto")(function* (
  store: ConfigStore, organizationId: string, payload: GithubPushReceivedEventData, repository: GithubResolvedRepository, head: string,
) {
  const { merged } = yield* storeCall(() => store.pendingSaves(organizationId, payload.repositoryId, payload.branch));
  const carried: string[] = [];
  for (const commit of merged) {
    if (yield* descendsFrom(payload.installationId, repository, commit, head)) carried.push(commit);
  }
  return carried;
});

/** The Deployments an observation admitted, as Cloud dispatches them to runners. */
function admitted(organizationId: string, written: ConfigWritten): ConfigDeploymentAdmittedEventData[] {
  if (written.written !== "automated") return [];
  return written.admitted.map((deployed) => ({
    organizationId, environmentId: deployed.environment, deploymentId: deployed.deployment.id,
  }));
}

/**
 * Tell one Organization's Store where the branch is now, compared from the head the Store last saw. A push another
 * worker applied first makes the Store refuse the stale base, so it reads and compares again.
 */
const observeBranchFor = Effect.fn("StoreGithub.observeBranchFor")(function* (
  store: ConfigStore, organizationId: string, payload: GithubPushReceivedEventData,
  repository: GithubResolvedRepository, head: string | null,
) {
  yield* freezeMerged(store, organizationId, payload);
  const merged = head === null ? [] : yield* mergedInto(store, organizationId, payload, repository, head);
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const base = yield* storeCall(() => store.branchHead(organizationId, payload.repositoryId, payload.branch));
    // No changed paths (a force-push, diverged or long history) deploys every Service that follows the branch.
    let changed: string[] | null = null;
    if (base !== null && head !== null && base !== head && !payload.forced) {
      changed = yield* compareInstallationRepositoryCommits(payload.installationId, repository, base, head).pipe(
        Effect.map((comparison) =>
          comparison.status === "ahead" && comparison.pathsComplete ? [...comparison.changedPaths] : null),
        Effect.catchIf(isGithubObservationNotFound, () => Effect.succeed(null)),
      );
    }
    const event: SystemEvent = {
      event: "branch_head", repository_id: payload.repositoryId, branch: payload.branch, base, head, changed, merged,
    };
    const written = yield* storeCall(() => store.system(organizationId, event)).pipe(
      Effect.map((written) => ({ written })),
      Effect.catchIf(isConflict, () => Effect.succeed(null)),
    );
    if (written !== null) return admitted(organizationId, written.written);
  }
  return yield* new StoreGithubFailure({ cause: "The branch's head kept moving while Cloud compared it." });
});

/**
 * A push, as each Organization of the installation's members sees it: Cloud reads the branch's head from GitHub now,
 * never the webhook's, so a late delivery can't bring back an older head. Resolves to the Deployments to dispatch.
 */
export const observeStoreBranch = Effect.fn("StoreGithub.observeBranch")(function* (payload: GithubPushReceivedEventData) {
  const store = yield* cloudStore;
  const organizations = yield* listGithubInstallationOrganizationIds(payload.installationId);
  if (organizations.length === 0) return [];
  const repository = yield* resolveGithubRepository(payload.installationId, payload.repositoryId);
  const live = yield* resolveGithubBranchHead(payload.installationId, repository, payload.ref);
  const head = live.state === "present" ? live.headSha : null;
  const deployments: ConfigDeploymentAdmittedEventData[] = [];
  for (const organizationId of organizations) {
    deployments.push(...yield* observeBranchFor(store, organizationId, payload, repository, head));
  }
  return deployments;
});

/**
 * A check suite's result as GitHub has it now, for each Organization of the installation's members; the Store keeps
 * the newest and lets a deploy waiting for the commit's CI go. Resolves to the Deployments to dispatch.
 */
export const observeStoreCheckSuite = Effect.fn("StoreGithub.observeCheckSuite")(function* (
  payload: GithubCheckSuiteReceivedEventData,
) {
  const store = yield* cloudStore;
  const organizations = yield* listGithubInstallationOrganizationIds(payload.installationId);
  if (organizations.length === 0) return [];
  const repository = yield* resolveGithubRepository(payload.installationId, payload.repositoryId);
  const suite = yield* fetchInstallationCheckSuite(payload.installationId, repository, payload.checkSuiteId);
  const event: SystemEvent = {
    event: "check_suite", repository_id: payload.repositoryId, suite: suite.checkSuiteId, head: suite.headSha,
    status: suite.status, conclusion: suite.conclusion, updated: githubTimestamp(suite.updatedAt),
  };
  const deployments: ConfigDeploymentAdmittedEventData[] = [];
  for (const organizationId of organizations) {
    const written = yield* storeCall(() => store.system(organizationId, event));
    deployments.push(...admitted(organizationId, written));
  }
  return deployments;
});
