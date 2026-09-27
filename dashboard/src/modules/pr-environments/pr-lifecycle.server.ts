import "@tanstack/react-start/server-only";
import { and, eq, inArray, ne, sql } from "drizzle-orm";
import { Effect } from "effect";
import { closePrEnvironment } from "#/modules/branches/branch-close.server";
import { writeAndDeploy } from "#/modules/branches/branch-operations.server";
import { branchSetupCommands, defaultBranchName } from "#/modules/branches/branch-plan";
import { service } from "#/modules/environment-design/tables";
import { fetchInstallationPullRequest } from "#/modules/github/github-observation.api";
import { environment, project } from "#/modules/project/tables";
import { activeTeardownFor } from "#/modules/runtime/teardown.repository";
import { Database } from "#/server/database.server";
import { carryInWaitingTriggers, landAtMerge, settleAtClose } from "./land.server";
import { fromRepository, prEnvironmentIntent } from "./pull-request";
import { actingMember } from "./plan-operations.server";
import { defaultPrEnvironmentPlan, prPlanInput } from "./repositories";
import { conditionalSave, prEnvironment, prEnvironmentPlan } from "./tables";

type Plan = typeof prEnvironmentPlan.$inferSelect;
type LivePullRequest = Effect.Success<ReturnType<typeof fetchInstallationPullRequest>>;

/**
 * Brings every project's PR Environment for one pull request in line with the pull request as GitHub has it now, so a
 * late or repeated delivery never undoes a newer state. Open: each project with PR Environments on makes one if it has
 * none, even while an old one is still being torn down. Closed: its approvals land if it merged, or drop, and each is
 * torn down where "remove its environment" is on. Every PR Environment's recorded facts refresh.
 */
export const applyPullRequest = Effect.fn("PrEnvironments.applyPullRequest")(function* (input: {
  installationId: number; repositoryId: number; number: number;
}) {
  const { drizzle } = yield* Database;
  const plans = yield* drizzle.select().from(prEnvironmentPlan)
    .where(and(eq(prEnvironmentPlan.installationId, input.installationId), eq(prEnvironmentPlan.repositoryId, input.repositoryId)));
  const ofPullRequest = and(eq(prEnvironment.repositoryId, input.repositoryId), eq(prEnvironment.number, input.number));
  const all = yield* drizzle.select({ environmentId: prEnvironment.environmentId, projectId: prEnvironment.projectId })
    .from(prEnvironment).where(ofPullRequest);
  // Nothing to act on: skip the read from GitHub.
  if (plans.length === 0 && all.length === 0) return "ignored_pull_request" as const;
  const live = yield* fetchInstallationPullRequest(input.installationId, input.repositoryId, input.number);
  // One being torn down is done with: closed, it keeps its facts, and a new one can start beside it.
  const closing = yield* activeTeardownFor(all.map((row) => row.environmentId));
  if (closing.size) yield* drizzle.update(prEnvironment).set({ closed: true }).where(inArray(prEnvironment.environmentId, [...closing]));
  const existing = all.filter((row) => !closing.has(row.environmentId));
  if (existing.length) {
    yield* drizzle.update(prEnvironment).set({
      title: live.title, author: live.author.login, headBranch: live.headBranch, targetBranch: live.targetBranch,
      commits: live.commits, closed: !live.open,
    }).where(inArray(prEnvironment.environmentId, existing.map((row) => row.environmentId)));
    // A new target Git branch withdraws every approval of the PR Environment.
    yield* drizzle.delete(conditionalSave).where(and(
      inArray(conditionalSave.prEnvironmentId, existing.map((row) => row.environmentId)), ne(conditionalSave.targetBranch, live.targetBranch),
    ));
  }

  if (!live.open) {
    // Approvals freeze or drop before any teardown; Destinations that don't deploy on push take theirs now.
    yield* settleAtClose(existing.map((row) => row.environmentId),
      live.mergeCommitSha ? { commitSha: live.mergeCommitSha, targetBranch: live.targetBranch } : null);
    if (live.mergeCommitSha) {
      yield* landAtMerge(input);
      yield* carryInWaitingTriggers(input);
    }
    for (const row of existing) {
      const { removeOnClose } = plans.find((plan) => plan.projectId === row.projectId) ?? defaultPrEnvironmentPlan;
      if (removeOnClose) yield* closePrEnvironment(row.environmentId);
    }
    return existing.length > 0 ? "pull_request_projected" as const : "ignored_pull_request" as const;
  }

  let created = false;
  let nothingFromRepository = false;
  for (const plan of plans) {
    if (!plan.enabled || plan.startFromEnvironmentId === null) continue;
    if (live.author.isBot && !plan.includeBots) continue;
    if (existing.some((row) => row.projectId === plan.projectId)) continue;
    const userId = yield* actingMember(plan);
    if (!userId) continue;
    const [start] = yield* drizzle.select({ intent: environment.intent }).from(environment)
      .where(eq(environment.id, plan.startFromEnvironmentId));
    if (!start?.intent.services.some((node) => fromRepository(node.config, input.repositoryId))) {
      nothingFromRepository = true;
      continue;
    }
    yield* createPrEnvironment(plan, plan.startFromEnvironmentId, { userId }, { ...live, repositoryId: input.repositoryId, number: input.number }).pipe(
      Effect.map(() => { created = true; }),
      // A refused plan: nothing to retry until someone changes it.
      Effect.catchTags({ Conflict: logNotCreated(plan), Validation: logNotCreated(plan) }),
    );
  }
  if (created || existing.length > 0) return "pull_request_projected" as const;
  return nothingFromRepository ? "ignored_nothing_from_repository" as const : "ignored_pull_request" as const;
});

const logNotCreated = (plan: Plan) => (error: { message: string }) =>
  Effect.logWarning("A PR Environment was not created.", { projectId: plan.projectId, reason: error.message });

/**
 * The system makes a pull request's PR Environment as a Branch of the plan's start-from Environment, acting for
 * `actor`, and admits its first deployment. It's named `pr-<number>`, or `pr-<number>-2`… when that's taken (another
 * repository's pull request, or an old one still being torn down). After derivation the repository's services track
 * the pull request's head Git branch and deploy on push, and every Own Copy runs one replica.
 */
const createPrEnvironment = Effect.fn("PrEnvironments.createPrEnvironment")(function* (
  plan: Plan, parentEnvironmentId: string, actor: { userId: string },
  pullRequest: LivePullRequest & { repositoryId: number; number: number },
) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ environment, project }).from(environment)
    .innerJoin(project, eq(project.id, environment.projectId))
    .where(eq(environment.id, parentEnvironmentId));
  if (!row) return;
  const taken = yield* drizzle.select({ namespace: environment.namespace }).from(environment)
    .where(eq(environment.organizationId, row.project.organizationId));
  const name = defaultBranchName(row.project.slug, `pr-${pullRequest.number}`, new Set(taken.map((environment) => environment.namespace)));
  const { repositoryId } = pullRequest;
  return yield* writeAndDeploy({
    actor, project: row.project, parent: row.environment,
    input: { name, focus: [], picks: plan.picks, keep: false, deployNow: true, setupCommands: plan.setupCommands },
    kind: {
      // Picks the start-from lacks are kept, and Then run commands for services it doesn't copy are left out.
      plan: (parent, deployed) => prPlanInput(parent, deployed, repositoryId, plan.picks),
      setupCommands: (branchPlan) => branchSetupCommands(branchPlan, plan.setupCommands),
      derive: (intent) => prEnvironmentIntent(intent, { repositoryId, headBranch: pullRequest.headBranch }),
      finish: ({ branch, next }) => Effect.gen(function* () {
        const { drizzle } = yield* Database;
        yield* drizzle.insert(prEnvironment).values({
          environmentId: branch.environmentId, organizationId: branch.organizationId, projectId: branch.projectId,
          repositoryId, number: pullRequest.number, title: pullRequest.title, author: pullRequest.author.login,
          headBranch: pullRequest.headBranch, targetBranch: pullRequest.targetBranch, commits: pullRequest.commits,
        });
        // The repository's services deploy on push whatever the Parent's Deployment Policy says; Wait for CI and watch paths stay.
        const ids = next.services.filter((node) => fromRepository(node.config, repositoryId)).map((node) => node.id);
        if (ids.length) yield* drizzle.update(service).set({ policy: sql`${service.policy} || '{"autoDeploy":true}'::jsonb` }).where(inArray(service.id, ids));
      }),
    },
  });
});

export type PullRequestEffectRunner = <A, E extends Error>(
  effect: Effect.Effect<A, E, Effect.Services<ReturnType<typeof applyPullRequest>>>,
) => Promise<A>;
