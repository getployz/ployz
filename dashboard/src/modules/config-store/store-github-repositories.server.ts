import "@tanstack/react-start/server-only";

import type { AutoDeployRule } from "@ployz/sdk";
import { and, eq, inArray } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { callStoreAsMember, stopGithubInstallationBuilds } from "#/modules/config-store/config-store.server";
import { GITHUB_BUILD_WORKFLOW_FILE, type GithubBuildReadiness, type GithubBuildRepository, type GithubWorkflowTemplate } from "#/modules/github/github-build-workflow";
import { organizationAccess, type GithubAccess } from "#/modules/github/github-access";
import { checkGithubBuildWorkflow } from "#/modules/github/github-build.server";
import { GithubApi } from "#/modules/github/github-observation.api";
import { listGithubInstallationsForOrganization, unlinkGithubInstallation } from "#/modules/github/github.repository";
import { githubRepositoryCache, organizationGithubInstallation } from "#/modules/github/tables";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";

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

/**
 * Disconnect, from Organization › GitHub or `ployz github disconnect LOGIN`: the Organization lets go of the GitHub
 * account it links by that login (a `manage` call). The link goes first, so nothing new reads or builds through it;
 * then its builds running on GitHub through that installation are cancelled as Cloud and their Deployments fail with
 * the disconnect. A stop that fails is caught by the build walk's next look. Other Organizations linked to the
 * installation keep it. Uninstalling the App happens on GitHub.
 */
export const disconnectGithub = Effect.fn("GithubCli.disconnect")(function* (actor: Actor, organizationSlug: string, login: string) {
  const { origins } = (yield* AppConfig).github;
  const { organization } = yield* requireAccess(actor, organizationSlug, "github:connect");
  const installation = (yield* listGithubInstallationsForOrganization(organization.id))
    .find((linked) => linked.accountLogin.toLowerCase() === login.toLowerCase());
  if (installation === undefined) return yield* new NotFound({ message: `No GitHub account ${login} in this Organization.` });
  yield* unlinkGithubInstallation({ organizationId: organization.id, installationId: installation.installationId });
  yield* stopGithubInstallationBuilds(installation.installationId, [organization.id]).pipe(
    Effect.catch((error) => Effect.logWarning("Could not stop a disconnected installation's GitHub builds.", error)),
  );
  const settings = installation.accountType === "Organization"
    ? `${origins.web}/organizations/${installation.accountLogin}/settings/installations/${installation.installationId}`
    : `${origins.web}/settings/installations/${installation.installationId}`;
  return { disconnected: { id: installation.installationId, account: installation.accountLogin }, uninstall_url: settings };
});
