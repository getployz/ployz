import "@tanstack/react-start/server-only";

import type { AutoDeployRule } from "@ployz/sdk";
import { and, eq, inArray } from "drizzle-orm";
import { Effect, Schema } from "effect";
import {
  GITHUB_BUILD_WORKFLOW_FILE,
  type GithubBuildReadiness,
  type GithubBuildRepository,
  type GithubWorkflowTemplate,
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
import { githubRepositoryCache, organizationGithubInstallation } from "#/modules/github/tables";
import { Database } from "#/server/database.server";

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
 * workflows on the default branch, so `active` means committed there and not disabled. `outdated` means active, but
 * not a file Ployz offers; `template` names the one it is. `dispatchable` says whoever holds the App's authority
 * dispatches the file there.
 */
export const checkGithubBuildWorkflow = Effect.fn("Github.checkBuildWorkflow")(
  function* (access: GithubAccess, repositoryId: number) {
    const workflow = yield* (yield* GithubAuthority).buildWorkflow(yield* repositoryOf(access, repositoryId)).pipe(
      Effect.catchIf(cannotRun, () => Effect.succeed(null)),
    );
    if (workflow === null) return { fullName: null, defaultBranch: null, readiness: "no_permission" as const, template: null, dispatchable: false };
    const readiness: GithubBuildReadiness = workflow.state !== "active" ? "setup_needed" : workflow.template !== null ? "ready" : "outdated";
    return {
      fullName: workflow.fullName, defaultBranch: workflow.defaultBranch, readiness, template: workflow.template, dispatchable: workflow.dispatchable,
    };
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
 * rule. Its only input from Cloud is `commit`: a rule's file declares no `build`.
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

/** `ruleBranches`: the branches automatic deploy rules build from it, none when no rule names it. */
export type StoreRepository = { installationId: number; repositoryId: number; fullName: string; ruleBranches: string[] };

/** Whether the Environment's automatic deploy rule is on, and the repository it builds from. */
const ruleSource = (view: { rule: { body: string } | null }) => {
  if (view.rule === null) return null;
  try {
    // SAFETY: the Store keeps only a rule body it parsed as an `AutoDeployRule`.
    const rule = JSON.parse(view.rule.body) as AutoDeployRule;
    return rule.ended ? null : rule.source;
  } catch {
    return null;
  }
};

/**
 * The GitHub repositories the Organization's Services build from: each Project's PR plans list theirs, and each
 * Environment's automatic deploy rule its own. A rule's repository takes the installation Cloud last saw reach it.
 */
const listStoreGithubRepositories = Effect.fn("Github.listStoreRepositories")(function* (actor: Actor, organizationId: string, organizationSlug: string) {
  const projects = yield* callStoreAsMember(actor, organizationSlug, { operation: "read", query: { query: "projects" } });
  if (!projects.ok) return [];
  const read = { concurrency: 4 } as const;
  const [plans, rules] = yield* Effect.all([
    Effect.forEach(projects.value.projects, (project) =>
      callStoreAsMember(actor, organizationSlug, { operation: "read", query: { query: "pr_plans", project: project.name } }), read),
    Effect.forEach(projects.value.projects.flatMap((project) => project.environments.map((environment) => ({ project: project.name, environment }))),
      (environment) => callStoreAsMember(actor, organizationSlug, { operation: "read", query: { query: "rule", environment } }), read),
  ], { concurrency: 2 });
  const found = new Map<number, StoreRepository>();
  for (const result of plans) {
    if (!result.ok) continue;
    for (const plan of result.value.plans) {
      found.set(plan.repository_id, { installationId: plan.installation_id, repositoryId: plan.repository_id, fullName: plan.repository, ruleBranches: [] });
    }
  }
  const ruled = new Map<number, { fullName: string; branches: Set<string> }>();
  for (const result of rules) {
    const source = result.ok ? ruleSource(result.value) : null;
    if (source === null) continue;
    const kept = ruled.get(source.repository_id) ?? { fullName: source.repository, branches: new Set<string>() };
    ruled.set(source.repository_id, { ...kept, branches: kept.branches.add(source.branch) });
  }
  const unplanned = [...ruled.keys()].filter((repositoryId) => !found.has(repositoryId));
  const { drizzle } = yield* Database;
  const installed = unplanned.length === 0 ? [] : yield* drizzle
    .select({ installationId: githubRepositoryCache.installationId, repositoryId: githubRepositoryCache.repositoryId })
    .from(githubRepositoryCache)
    .innerJoin(organizationGithubInstallation, and(
      eq(organizationGithubInstallation.installationId, githubRepositoryCache.installationId),
      eq(organizationGithubInstallation.organizationId, organizationId),
    ))
    .where(inArray(githubRepositoryCache.repositoryId, unplanned))
    .orderBy(githubRepositoryCache.installationId);
  for (const { installationId, repositoryId } of installed) {
    if (!found.has(repositoryId)) found.set(repositoryId, { installationId, repositoryId, fullName: ruled.get(repositoryId)?.fullName ?? "", ruleBranches: [] });
  }
  for (const [repositoryId, rule] of ruled) {
    const repository = found.get(repositoryId);
    if (repository !== undefined) repository.ruleBranches = [...rule.branches].sort();
  }
  return [...found.values()];
});

const contentsSchema = Schema.Struct({ content: Schema.String, encoding: Schema.Literal("base64") });

/**
 * Whether the repository's build workflow calls a reusable workflow, as a rule's does: its pushes may build under a
 * rule Cloud can't read, such as a preview rule, which the build template would stop.
 */
const callsReusableWorkflow = Effect.fn("Github.callsReusableWorkflow")(function* (access: GithubAccess, repositoryId: number) {
  const file = yield* (yield* GithubApi).json({
    access, path: `/repositories/${repositoryId}/contents/.github/workflows/${GITHUB_BUILD_WORKFLOW_FILE}`, operation: "fetch_contents", schema: contentsSchema,
  });
  return /^\s*uses:\s*\S+\/\.github\/workflows\/\S+@/m.test(Buffer.from(file.content, "base64").toString("utf8"));
});

/**
 * The file Ployz offers a repository: the rule template when its pushes build under a rule, which an automatic deploy
 * rule names, its file already is, or its file looks like; else the build template. Offering the build template there
 * would stop its push builds.
 */
const offeredTemplate = Effect.fn("Github.offeredTemplate")(function* (
  access: GithubAccess, repository: StoreRepository, checked: { readiness: GithubBuildReadiness; template: GithubWorkflowTemplate | null },
) {
  if (repository.ruleBranches.length > 0 || checked.template === "rule") return "rule";
  if (checked.readiness !== "outdated") return "build";
  const reusable = yield* callsReusableWorkflow(access, repository.repositoryId).pipe(Effect.orElseSucceed(() => false));
  return reusable ? "rule" : "build";
});

const disconnected = { fullName: null, defaultBranch: null, readiness: "no_permission" as const, template: null, dispatchable: false };

/** One repository's row on Organization → Builds: the file Ployz offers it, and whether it holds that file. */
export const checkGithubBuildRepository = Effect.fn("Github.checkBuildRepository")(function* (organizationId: string, repository: StoreRepository) {
  const access = organizationAccess(organizationId, repository.installationId);
  const checked = yield* checkGithubBuildWorkflow(access, repository.repositoryId).pipe(
    Effect.catchTag("GithubDisconnected", () => Effect.succeed(disconnected)),
  );
  const template = yield* offeredTemplate(access, repository, checked);
  return {
    installationId: repository.installationId,
    repositoryId: repository.repositoryId,
    fullName: checked.fullName ?? repository.fullName,
    defaultBranch: checked.defaultBranch,
    // The other template is one Ployz offers, but not to this repository.
    readiness: checked.readiness === "ready" && checked.template !== template ? "outdated" : checked.readiness,
    template,
    ruleBranch: repository.ruleBranches.length === 1 ? repository.ruleBranches[0] ?? null : null,
  } satisfies GithubBuildRepository;
});

export const listGithubBuildRepositories = Effect.fn("Github.listBuildRepositories")(
  function* (actor: Actor, input: { organizationSlug: string }) {
    const { organization } = yield* requireAccess(actor, input.organizationSlug, "project:read");
    const repositories = yield* listStoreGithubRepositories(actor, organization.id, input.organizationSlug);
    return yield* Effect.forEach(repositories, (repository) => checkGithubBuildRepository(organization.id, repository), { concurrency: 4 });
  },
);
