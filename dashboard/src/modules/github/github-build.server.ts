import "@tanstack/react-start/server-only";

import { Effect, Schema } from "effect";
import {
  GITHUB_BUILD_WORKFLOW_FILE,
  type GithubBuildReadiness,
  type GithubBuildRepository,
} from "#/modules/github/github-build-workflow";
import { githubIdSchema, githubRepositoryFullNameSchema } from "#/modules/github/github-ingestion.contracts";
import { GithubApi, GithubObservationError } from "#/modules/github/github-observation.api";
import type { Actor } from "#/modules/identity/actor";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { callStoreAsMember } from "#/modules/config-store/config-store.server";

const repositorySchema = Schema.Struct({
  id: githubIdSchema,
  full_name: githubRepositoryFullNameSchema,
  default_branch: Schema.NonEmptyString,
});
const workflowSchema = Schema.Struct({ path: Schema.String, state: Schema.String });

/** A missing permission is a 403 GitHub won't lift by waiting; rate limits are retriable 403s. */
function lacksPermission(error: GithubObservationError) {
  return error.status === 403 && !error.retriable;
}

/**
 * Whether GitHub will run the build workflow in this repository now. Cloud calls it for the
 * Servers page and again right before dispatching a build. The Actions API only lists
 * workflows on the default branch, so `active` means committed there and not disabled.
 */
export const checkGithubBuildWorkflow = Effect.fn("Github.checkBuildWorkflow")(
  function* (installationId: number, repositoryId: number) {
    const api = yield* GithubApi;
    const repository = yield* api.json({
      installationId,
      url: `https://api.github.com/repositories/${repositoryId}`,
      operation: "resolve_repository",
      schema: repositorySchema,
    }).pipe(
      // The installation no longer reaches this repository.
      Effect.catchIf((error) => error.code === "not_found" || lacksPermission(error), () => Effect.succeed(null)),
    );
    if (repository === null) return { fullName: null, defaultBranch: null, readiness: "no_permission" as const };
    const readiness: GithubBuildReadiness = yield* api.json({
      installationId,
      url: `https://api.github.com/repos/${repository.full_name}/actions/workflows/${GITHUB_BUILD_WORKFLOW_FILE}`,
      operation: "fetch_workflow",
      schema: workflowSchema,
    }).pipe(
      Effect.map((workflow) =>
        workflow.state === "active" && workflow.path === `.github/workflows/${GITHUB_BUILD_WORKFLOW_FILE}` ? "ready" as const : "setup_needed" as const),
      Effect.catchIf((error) => error.code === "not_found", () => Effect.succeed("setup_needed" as const)),
      // Installations that haven't accepted Actions access get a 403 here.
      Effect.catchIf(lacksPermission, () => Effect.succeed("no_permission" as const)),
    );
    return { fullName: repository.full_name, defaultBranch: repository.default_branch, readiness };
  },
);

const dispatchedRunSchema = Schema.Struct({ workflow_run_id: githubIdSchema, html_url: Schema.String });

/**
 * Dispatch the build workflow on the default branch and learn its run. Inputs are visible on
 * GitHub, so they never carry secrets: the runner fetches those at check-in.
 */
export const dispatchGithubBuildWorkflow = Effect.fn("Github.dispatchBuildWorkflow")(function* (input: {
  installationId: number; fullName: string; defaultBranch: string;
  inputs: { build: string; cloud: string; runner: string };
}) {
  const api = yield* GithubApi;
  const run = yield* api.json({
    installationId: input.installationId,
    url: `https://api.github.com/repos/${input.fullName}/actions/workflows/${GITHUB_BUILD_WORKFLOW_FILE}/dispatches`,
    operation: "dispatch_workflow",
    schema: dispatchedRunSchema,
    method: "POST",
    body: { ref: input.defaultBranch, inputs: input.inputs, return_run_details: true },
  });
  return {
    runId: run.workflow_run_id,
    runUrl: run.html_url,
    workflowRef: `${input.fullName}/.github/workflows/${GITHUB_BUILD_WORKFLOW_FILE}@refs/heads/${input.defaultBranch}`,
  };
});

/** Cancel a build run. A run that already finished can't be cancelled; that's fine. */
export const cancelGithubRun = Effect.fn("Github.cancelRun")(function* (input: { installationId: number; fullName: string; runId: number }) {
  const api = yield* GithubApi;
  yield* api.json({
    installationId: input.installationId,
    url: `https://api.github.com/repos/${input.fullName}/actions/runs/${input.runId}/cancel`,
    operation: "cancel_run",
    schema: Schema.Unknown,
    method: "POST",
  }).pipe(Effect.catchIf((error) => error.status === 409, () => Effect.void));
});

const runStatusSchema = Schema.Struct({ status: Schema.String });

/** Whether a build run has completed, whatever its conclusion. */
export const githubRunCompleted = Effect.fn("Github.runCompleted")(function* (input: { installationId: number; fullName: string; runId: number }) {
  const api = yield* GithubApi;
  const run = yield* api.json({
    installationId: input.installationId,
    url: `https://api.github.com/repos/${input.fullName}/actions/runs/${input.runId}`,
    operation: "fetch_run",
    schema: runStatusSchema,
  });
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
    yield* requireInfrastructureOrganization(actor, input.organizationSlug);
    const repositories = yield* listStoreGithubRepositories(actor, input.organizationSlug);
    return yield* Effect.forEach(repositories, (repository) =>
      checkGithubBuildWorkflow(repository.installationId, repository.repositoryId).pipe(
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
