import "@tanstack/react-start/server-only";

import { Effect, Schema } from "effect";
import {
  GITHUB_BUILD_WORKFLOW_FILE,
  type GithubBuildReadiness,
  type GithubBuildRepository,
} from "#/modules/github/github-build-workflow";
import { GithubAuthority } from "#/modules/github/github-authority.server";
import { githubIdSchema, githubRepositoryFullNameSchema } from "#/modules/github/github-ingestion.contracts";
import { organizationAccess, type GithubAccess } from "#/modules/github/github-access";
import {
  GithubApi, GithubObservationError, githubLacksPermission, isGithubObservationNotFound, type GithubReadError,
} from "#/modules/github/github-observation.api";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";
import { callStoreAsMember } from "#/modules/config-store/config-store.server";

const repositoryIdSchema = Schema.Struct({ id: githubIdSchema, full_name: githubRepositoryFullNameSchema });

/** The installation's repository, as the authority names it, for an `access` Cloud may act through now. */
const repositoryOf = Effect.fn("Github.repositoryOf")(function* (access: GithubAccess, repositoryId: number) {
  return { installationId: yield* (yield* GithubApi).installationOf(access), repositoryId };
});

/**
 * The ID of the repository a dispatched run records by name. A rename redirects; the minter then checks the run is
 * this repository's.
 */
const repositoryNamed = Effect.fn("Github.repositoryNamed")(function* (access: GithubAccess, fullName: string) {
  const api = yield* GithubApi;
  const [owner = "", name = ""] = fullName.split("/");
  const found = yield* api.json({
    access, path: `/repos/${encodeURIComponent(owner)}/${encodeURIComponent(name)}`, operation: "resolve_repository", schema: repositoryIdSchema,
  });
  return yield* repositoryOf(access, found.id);
});

/** An installation that no longer reaches the repository, or never accepted Actions: GitHub won't build here. */
const cannotRun = (error: GithubReadError) => isGithubObservationNotFound(error) || githubLacksPermission(error);

/**
 * Whether GitHub will run the build workflow in this repository now. Cloud calls it for the
 * Servers page and again right before dispatching a build. The Actions API only lists
 * workflows on the default branch, so `active` means committed there and not disabled.
 * `dispatchable` says whoever holds the App's authority dispatches the file there.
 */
export const checkGithubBuildWorkflow = Effect.fn("Github.checkBuildWorkflow")(
  function* (access: GithubAccess, repositoryId: number) {
    const workflow = yield* (yield* GithubAuthority).buildWorkflow(yield* repositoryOf(access, repositoryId)).pipe(
      Effect.catchIf(cannotRun, () => Effect.succeed(null)),
    );
    if (workflow === null) return { fullName: null, defaultBranch: null, readiness: "no_permission" as const, dispatchable: false };
    const readiness: GithubBuildReadiness = workflow.state === "active" ? "ready" : "setup_needed";
    return { fullName: workflow.fullName, defaultBranch: workflow.defaultBranch, readiness, dispatchable: workflow.dispatchable };
  },
);

/**
 * Dispatch the build workflow on `branch` and learn its run. Inputs are visible on GitHub, so they
 * never carry secrets: the runner fetches those at check-in. Whoever holds the App's authority
 * fills `cloud`, and refuses a branch whose workflow file is not one Ployz offers.
 */
export const dispatchGithubBuildWorkflow = Effect.fn("Github.dispatchBuildWorkflow")(function* (input: {
  access: GithubAccess; repositoryId: number; branch: string; inputs: { build: string; runner: string };
}) {
  const repository = yield* repositoryOf(input.access, input.repositoryId);
  return yield* (yield* GithubAuthority).dispatchBuild({ ...repository, branch: input.branch, inputs: input.inputs });
});

/**
 * Dispatch the rule's build workflow on `branch` to build `commit`, a commit a person approved or reviewed under the
 * rule. The rule's workflow names Cloud's origin itself, and declares only the commit input.
 */
export const dispatchApprovedGithubBuild = Effect.fn("Github.dispatchApprovedBuild")(function* (input: {
  access: GithubAccess; repositoryId: number; branch: string; workflow: string; commit: string;
}) {
  if (input.workflow !== `.github/workflows/${GITHUB_BUILD_WORKFLOW_FILE}`) {
    return yield* new GithubObservationError({
      code: "invalid_argument", operation: "dispatch_workflow", retriable: false, message: `Ployz dispatches only .github/workflows/${GITHUB_BUILD_WORKFLOW_FILE}.`,
    });
  }
  const repository = yield* repositoryOf(input.access, input.repositoryId);
  return yield* (yield* GithubAuthority).dispatchApprovedBuild({ ...repository, branch: input.branch, commit: input.commit });
});

/**
 * The run a push of `commit` started of the workflow at `workflow` (its path in the repository), if GitHub has started
 * it: a build under an automatic deploy rule runs there, not on a run Cloud dispatches. Ployz follows only its build
 * workflow.
 */
export const findGithubPushRun = Effect.fn("Github.findPushRun")(function* (input: {
  access: GithubAccess; repositoryId: number; workflow: string; commit: string;
}) {
  if (input.workflow !== `.github/workflows/${GITHUB_BUILD_WORKFLOW_FILE}`) {
    return yield* new GithubObservationError({
      code: "invalid_argument", operation: "list_push_runs", retriable: false, message: `Ployz follows only .github/workflows/${GITHUB_BUILD_WORKFLOW_FILE}.`,
    });
  }
  const repository = yield* repositoryOf(input.access, input.repositoryId);
  const runs = yield* (yield* GithubAuthority).buildRuns({ ...repository, headSha: input.commit, event: "push" });
  const run = runs.find((found) => found.headSha === input.commit && found.event === "push");
  return run === undefined ? null : { runId: run.id, runUrl: run.url, completed: run.status === "completed" };
});

export type GithubRunRef = { access: GithubAccess; fullName: string; runId: number };

/** Cancel a build run. A run that already finished can't be cancelled; that's fine. */
export const cancelGithubRun = Effect.fn("Github.cancelRun")(function* (input: GithubRunRef) {
  const repository = yield* repositoryNamed(input.access, input.fullName);
  yield* (yield* GithubAuthority).cancelBuild({ ...repository, runId: input.runId });
});

/**
 * Run a completed build run again. Its token keeps the push's commit, event and actor, so a push run that ended before
 * Cloud followed it, as when its pull request opened later, builds on its next attempt.
 */
export const rerunGithubRun = Effect.fn("Github.rerunRun")(function* (input: { access: GithubAccess; repositoryId: number; runId: number }) {
  const repository = yield* repositoryOf(input.access, input.repositoryId);
  yield* (yield* GithubAuthority).rerunBuild({ ...repository, runId: input.runId });
});

/** Whether a build run has completed, whatever its conclusion. */
export const githubRunCompleted = Effect.fn("Github.runCompleted")(function* (input: GithubRunRef) {
  const repository = yield* repositoryNamed(input.access, input.fullName);
  const run = yield* (yield* GithubAuthority).buildRun({ ...repository, runId: input.runId });
  return run.status === "completed";
});

/** The GitHub repositories the Organization's Services build from: each Project's PR plans list its repositories. */
const listStoreGithubRepositories = Effect.fn("Github.listStoreRepositories")(function* (actor: Actor, organizationSlug: string) {
  const projects = yield* callStoreAsMember(actor, organizationSlug, { operation: "read", query: { query: "projects" } });
  if (!projects.ok) return [];
  const plans = yield* Effect.forEach(projects.value.projects, (project) =>
    callStoreAsMember(actor, organizationSlug, { operation: "read", query: { query: "pr_plans", project: project.name } }), { concurrency: 4 });
  const found = new Map<string, { installationId: number; repositoryId: number; fullName: string }>();
  for (const result of plans) {
    if (!result.ok) continue;
    for (const plan of result.value.plans) {
      found.set(`${plan.installation_id}:${plan.repository_id}`,
        { installationId: plan.installation_id, repositoryId: plan.repository_id, fullName: plan.repository });
    }
  }
  return [...found.values()];
});

export const listGithubBuildRepositories = Effect.fn("Github.listBuildRepositories")(
  function* (actor: Actor, input: { organizationSlug: string }) {
    const { organization } = yield* requireAccess(actor, input.organizationSlug, "project:read");
    const repositories = yield* listStoreGithubRepositories(actor, input.organizationSlug);
    return yield* Effect.forEach(repositories, (repository) =>
      checkGithubBuildWorkflow(organizationAccess(organization.id, repository.installationId), repository.repositoryId).pipe(
        Effect.catchTag("GithubDisconnected", () => Effect.succeed({ fullName: null, defaultBranch: null, readiness: "no_permission" as const, dispatchable: false })),
        Effect.map((checked): GithubBuildRepository => ({
          installationId: repository.installationId,
          repositoryId: repository.repositoryId,
          fullName: checked.fullName ?? repository.fullName,
          defaultBranch: checked.defaultBranch,
          readiness: checked.readiness,
        })),
      ), { concurrency: 4 });
  },
);
