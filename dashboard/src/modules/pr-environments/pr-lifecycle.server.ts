import "@tanstack/react-start/server-only";
import { and, eq, inArray, ne } from "drizzle-orm";
import { Data, Effect } from "effect";
import { closePrEnvironment } from "#/modules/branches/branch-close.server";
import { createPrEnvironment } from "#/modules/branches/branch-operations.server";
import { fetchInstallationPullRequest } from "#/modules/github/github-observation.api";
import { environment, environmentBranch } from "#/modules/project/tables";
import { activeTeardownFor } from "#/modules/runtime/teardown.repository";
import { Database } from "#/server/database.server";
import { carryInWaitingTriggers, landAtMerge, settleAtClose } from "./land.server";
import { fromRepository } from "./pull-request";
import { conditionalSave, prEnvironmentPlan } from "./tables";

/** The old PR Environment is still being torn down; its namespace frees up once it's gone, so the delivery retries. */
export class PrEnvironmentStillClosing extends Data.TaggedError("PrEnvironmentStillClosing")<{ environmentId: string }> {
  override get message() {
    return "The pull request's previous PR Environment is still being torn down.";
  }
}

/**
 * Brings every project's PR Environment for one pull request in line with the pull request as GitHub has it now, so a
 * late or repeated delivery never undoes a newer state. Open: each project with PR Environments on makes one if it has
 * none. Closed: its approvals land if it merged, or drop, and each is torn down where "remove its environment" is on.
 * Every PR Environment's recorded facts refresh.
 */
export const applyPullRequest = Effect.fn("PrEnvironments.applyPullRequest")(function* (input: {
  installationId: number; repositoryId: number; number: number;
}) {
  const { drizzle } = yield* Database;
  const plans = yield* drizzle.select().from(prEnvironmentPlan)
    .where(and(eq(prEnvironmentPlan.installationId, input.installationId), eq(prEnvironmentPlan.repositoryId, input.repositoryId)));
  const ofPullRequest = and(eq(environmentBranch.prRepositoryId, input.repositoryId), eq(environmentBranch.prNumber, input.number));
  const [anyExisting] = yield* drizzle.select({ environmentId: environmentBranch.environmentId }).from(environmentBranch).where(ofPullRequest).limit(1);
  // Nothing to act on: skip the read from GitHub.
  if (plans.length === 0 && !anyExisting) return "ignored_pull_request" as const;
  const live = yield* fetchInstallationPullRequest(input.installationId, input.repositoryId, input.number);
  const existing = yield* drizzle.update(environmentBranch).set({
    prTitle: live.title, prAuthor: live.author.login,
    prHeadBranch: live.headBranch, prHeadSha: live.headSha, prTargetBranch: live.targetBranch,
  }).where(ofPullRequest).returning({ environmentId: environmentBranch.environmentId, projectId: environmentBranch.projectId });
  // A new target Git branch withdraws every approval of the PR Environment.
  if (existing.length) {
    yield* drizzle.delete(conditionalSave).where(and(
      inArray(conditionalSave.prEnvironmentId, existing.map((row) => row.environmentId)), ne(conditionalSave.targetBranch, live.targetBranch),
    ));
  }
  const closing = yield* activeTeardownFor(existing.map((row) => row.environmentId));

  if (!live.open) {
    // Approvals freeze or drop before any teardown; Destinations that don't deploy on push take theirs now.
    yield* settleAtClose(existing.map((row) => row.environmentId),
      live.mergeCommitSha ? { commitSha: live.mergeCommitSha, targetBranch: live.targetBranch } : null);
    if (live.mergeCommitSha) {
      yield* landAtMerge(input);
      yield* carryInWaitingTriggers(input);
    }
    for (const row of existing) {
      // No plan row reads as the defaults: removed when it closes.
      const removeOnClose = plans.find((plan) => plan.projectId === row.projectId)?.removeOnClose ?? true;
      if (removeOnClose && !closing.has(row.environmentId)) yield* closePrEnvironment(row.environmentId);
    }
    return existing.length > 0 ? "pull_request_projected" as const : "ignored_pull_request" as const;
  }

  let created = false;
  let nothingFromRepository = false;
  for (const plan of plans) {
    if (!plan.enabled || plan.enabledByUserId === null || plan.startFromEnvironmentId === null) continue;
    if (live.author.isBot && !plan.includeBots) continue;
    const own = existing.find((row) => row.projectId === plan.projectId);
    if (own && closing.has(own.environmentId)) return yield* new PrEnvironmentStillClosing({ environmentId: own.environmentId });
    if (own) continue;
    const [start] = yield* drizzle.select({ intent: environment.intent }).from(environment)
      .where(eq(environment.id, plan.startFromEnvironmentId));
    const focus = (start?.intent.services ?? [])
      .filter((node) => fromRepository(node.config, input.repositoryId)).map((node) => node.lineageId);
    if (focus.length === 0) {
      nothingFromRepository = true;
      continue;
    }
    // ponytail: acts for the member who turned PR Environments on, even if they've since left the organization.
    yield* createPrEnvironment({
      actor: { userId: plan.enabledByUserId },
      parentEnvironmentId: plan.startFromEnvironmentId,
      focus,
      picks: plan.picks,
      setupCommands: plan.setupCommands,
      pullRequest: {
        repositoryId: input.repositoryId, repository: plan.repository, number: input.number,
        title: live.title, author: live.author.login,
        headBranch: live.headBranch, headSha: live.headSha, targetBranch: live.targetBranch,
      },
    }).pipe(
      Effect.map(() => { created = true; }),
      // A taken name or a refused plan: nothing to retry until someone changes it.
      Effect.catchTags({
        Conflict: (error) => Effect.logWarning("A PR Environment was not created.", { projectId: plan.projectId, reason: error.message }),
        Validation: (error) => Effect.logWarning("A PR Environment was not created.", { projectId: plan.projectId, reason: error.message }),
      }),
    );
  }
  if (created || existing.length > 0) return "pull_request_projected" as const;
  return nothingFromRepository ? "ignored_nothing_from_repository" as const : "ignored_pull_request" as const;
});

export type PullRequestEffectRunner = <A, E extends Error>(
  effect: Effect.Effect<A, E, Effect.Services<ReturnType<typeof applyPullRequest>>>,
) => Promise<A>;
