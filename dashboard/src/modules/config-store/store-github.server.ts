import "@tanstack/react-start/server-only";
import type { ConfigStore, ConfigWritten, SystemEvent } from "@ployz/sdk";
import { Data, Effect } from "effect";
import { cloudStore } from "#/modules/config-store/config-store.server";
import type { StoreDeploymentServices } from "#/modules/config-store/store-deployment.server";
import {
  compareInstallationRepositoryCommits,
  fetchInstallationCheckSuite,
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

/** What Cloud's GitHub workers need to feed the Store. */
export type StoreGithubServices = StoreDeploymentServices;

export class StoreGithubFailure extends Data.TaggedError("StoreGithubFailure")<{ readonly cause: unknown }> {}

const storeCall = <A>(call: () => Promise<A>) =>
  Effect.tryPromise({ try: call, catch: (cause) => new StoreGithubFailure({ cause }) });

function isConflict(cause: unknown): cause is StoreGithubFailure {
  return cause instanceof StoreGithubFailure && typeof cause.cause === "object" && cause.cause !== null
    && "code" in cause.cause && cause.cause.code === "conflict";
}

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
      event: "branch_head", repository_id: payload.repositoryId, branch: payload.branch, base, head, changed,
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
    status: suite.status, conclusion: suite.conclusion, updated: new Date(suite.updatedAt).toISOString().replace(/\.\d{3}Z$/, "Z"),
  };
  const deployments: ConfigDeploymentAdmittedEventData[] = [];
  for (const organizationId of organizations) {
    const written = yield* storeCall(() => store.system(organizationId, event));
    deployments.push(...admitted(organizationId, written));
  }
  return deployments;
});
