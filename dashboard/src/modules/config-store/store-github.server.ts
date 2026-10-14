import "@tanstack/react-start/server-only";
import type { ConfigStore, SystemEvent } from "@ployz/sdk";
import { Data, Effect } from "effect";
import { admittedEvents, storeSystem } from "#/modules/config-store/config-store.server";
import { cloudStore, refusedWith, storeTry } from "#/modules/config-store/store-sdk.server";
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
import { organizationAccess, type GithubAccess } from "#/modules/github/github-access";
import { listLinkedOrganizationIds } from "#/modules/github/github.repository";
import type { ConfigDeploymentAdmittedEventData } from "#/modules/inngest/events";

/** GitHub told Cloud something it can't use yet; the worker retries. */
export class StoreGithubFailure extends Data.TaggedError("StoreGithubFailure")<{ readonly message: string }> {}

/** GitHub's `updated_at` as the Store orders facts by it. */
export const githubTimestamp = (at: string) => new Date(at).toISOString().replace(/\.\d{3}Z$/, "Z");

type LivePullRequest = Effect.Success<ReturnType<typeof fetchInstallationPullRequest>>;

/** A pull request's facts as the Store takes them, with `targetHead` from `mergedInto`. */
export function pullRequestEvent(
  repositoryId: number, number: number, live: LivePullRequest & { updatedAt: string }, targetHead: string | null = null,
): SystemEvent {
  return {
    event: "pull_request", repository_id: repositoryId, number, title: live.title,
    author: live.author.login, bot: live.author.isBot, head_branch: live.headBranch, head: live.headSha,
    target_branch: live.targetBranch, commits: live.commits, open: live.open, merge_commit: live.mergeCommitSha,
    target_head: targetHead, updated: githubTimestamp(live.updatedAt),
  };
}

/**
 * A merged pull request's target branch head now, when GitHub says it contains the merge commit. Two quick merges
 * push the branch past the first one's commit before Cloud reads it, so the Store never sees that commit as a head;
 * this tells it a head it saw already carries the merge.
 */
export const mergedInto = Effect.fn("StoreGithub.mergedInto")(function* (
  access: GithubAccess, repositoryId: number, live: Pick<LivePullRequest, "mergeCommitSha" | "targetBranch">,
) {
  if (!live.mergeCommitSha) return null;
  const repository = yield* resolveGithubRepository(access, repositoryId);
  const branch = yield* resolveGithubBranchHead(access, repository, `refs/heads/${live.targetBranch}`);
  if (branch.state !== "present") return null;
  const comparison = yield* compareInstallationRepositoryCommits(access, repository, live.mergeCommitSha, branch.headSha)
    .pipe(Effect.catchIf(isGithubObservationNotFound, () => Effect.succeed(null)));
  return comparison?.status === "ahead" || comparison?.status === "identical" ? branch.headSha : null;
});

/**
 * Pull requests into the pushed branch that merged before their closed delivery arrived: the Store hears it now, so
 * the merge lands before the push and deploys with it. Asks GitHub only when one awaits its merge. Resolves to the
 * Deployments the landing admitted.
 * ponytail: a PR Environment this closes leaves the Servers at the next sweep.
 */
const observeMerged = Effect.fn("StoreGithub.observeMerged")(function* (
  store: ConfigStore, access: GithubAccess, organizationId: string, payload: GithubPushReceivedEventData,
) {
  const awaiting = yield* storeTry(() => store.awaitingMerge(organizationId, payload.repositoryId, payload.branch));
  const deployments: ConfigDeploymentAdmittedEventData[] = [];
  for (const number of awaiting) {
    const live = yield* fetchInstallationPullRequest(access, payload.repositoryId, number).pipe(
      Effect.catchIf(isGithubObservationNotFound, () => Effect.succeed(null)),
    );
    const { updatedAt } = live ?? {};
    if (!live?.mergeCommitSha || live.targetBranch !== payload.branch || !updatedAt) continue;
    const targetHead = yield* mergedInto(access, payload.repositoryId, live);
    const written = yield* storeSystem(organizationId, pullRequestEvent(payload.repositoryId, number, { ...live, updatedAt }, targetHead));
    deployments.push(...admittedEvents(organizationId, written));
  }
  return deployments;
});

/**
 * Tell one Organization's Store where the branch is now, compared from the head the Store last saw. A push another
 * worker applied first makes the Store refuse the stale base, so it reads and compares again.
 */
const observeBranchFor = Effect.fn("StoreGithub.observeBranchFor")(function* (
  store: ConfigStore, access: GithubAccess, organizationId: string, payload: GithubPushReceivedEventData,
  repository: GithubResolvedRepository, head: string | null,
) {
  const merged = yield* observeMerged(store, access, organizationId, payload);
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const base = yield* storeTry(() => store.branchHead(organizationId, payload.repositoryId, payload.branch));
    // No changed paths (a force-push, diverged or long history) deploys every Service that follows the branch.
    let changed: string[] | null = null;
    if (base !== null && head !== null && base !== head && !payload.forced) {
      changed = yield* compareInstallationRepositoryCommits(access, repository, base, head).pipe(
        Effect.map((comparison) =>
          comparison.status === "ahead" && comparison.pathsComplete ? [...comparison.changedPaths] : null),
        Effect.catchIf(isGithubObservationNotFound, () => Effect.succeed(null)),
      );
    }
    const event: SystemEvent = {
      event: "branch_head", repository_id: payload.repositoryId, branch: payload.branch, base, head, changed,
    };
    const written = yield* storeSystem(organizationId, event).pipe(
      Effect.map((written) => ({ written })),
      Effect.catchIf(refusedWith("conflict"), () => Effect.succeed(null)),
    );
    if (written !== null) return [...merged, ...admittedEvents(organizationId, written.written)];
  }
  return yield* new StoreGithubFailure({ message: "The branch's head kept moving while Cloud compared it." });
});

/**
 * The Organizations linked to the installation, and the access the fan-out reads GitHub with: the first linked one's.
 * Every linked Organization shares the installation's one token, so one read serves them all.
 */
const linkedOrganizations = Effect.fn("StoreGithub.linkedOrganizations")(function* (installationId: number) {
  const organizations = yield* listLinkedOrganizationIds(installationId);
  const first = organizations[0];
  if (first === undefined) return null;
  return { organizations, access: organizationAccess(first, installationId) };
});

/**
 * A push, as each Organization linked to the installation sees it: Cloud reads the branch's head from GitHub now,
 * never the webhook's, so a late delivery can't bring back an older head. Resolves to the Deployments to dispatch.
 */
export const observeStoreBranch = Effect.fn("StoreGithub.observeBranch")(function* (payload: GithubPushReceivedEventData) {
  const store = yield* cloudStore;
  const linked = yield* linkedOrganizations(payload.installationId);
  if (linked === null) return [];
  const { access } = linked;
  const repository = yield* resolveGithubRepository(access, payload.repositoryId);
  const live = yield* resolveGithubBranchHead(access, repository, payload.ref);
  const head = live.state === "present" ? live.headSha : null;
  const deployments: ConfigDeploymentAdmittedEventData[] = [];
  for (const organizationId of linked.organizations) {
    deployments.push(...yield* observeBranchFor(store, organizationAccess(organizationId, payload.installationId), organizationId, payload, repository, head));
  }
  return deployments;
});

/**
 * A check suite's result as GitHub has it now, for each Organization linked to the installation; the Store keeps
 * the newest and lets a deploy waiting for the commit's CI go. Resolves to the Deployments to dispatch.
 */
export const observeStoreCheckSuite = Effect.fn("StoreGithub.observeCheckSuite")(function* (
  payload: GithubCheckSuiteReceivedEventData,
) {
  const linked = yield* linkedOrganizations(payload.installationId);
  if (linked === null) return [];
  const { access, organizations } = linked;
  const repository = yield* resolveGithubRepository(access, payload.repositoryId);
  const suite = yield* fetchInstallationCheckSuite(access, repository, payload.checkSuiteId);
  const event: SystemEvent = {
    event: "check_suite", repository_id: payload.repositoryId, suite: suite.checkSuiteId, head: suite.headSha,
    // GitHub's other statuses (requested, waiting, pending) are still running, as `other` reads.
    status: suite.status === "queued" || suite.status === "in_progress" || suite.status === "completed" ? suite.status : "other",
    conclusion: suite.conclusion, updated: githubTimestamp(suite.updatedAt),
  };
  const deployments: ConfigDeploymentAdmittedEventData[] = [];
  for (const organizationId of organizations) {
    const written = yield* storeSystem(organizationId, event);
    deployments.push(...admittedEvents(organizationId, written));
  }
  return deployments;
});
