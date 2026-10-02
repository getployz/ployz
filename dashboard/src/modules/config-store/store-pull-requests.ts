import type {
  EnvironmentRef, MoveQuery, PrPlan, PrPlansQuery, PullRequestHint, PullRequestQuery, PullRequestRef, PullRequestView,
} from "@ployz/sdk";
import { listNames, plural } from "#/lib/plural";

/** A Project's PR plans: one per repository its Services deploy from through the GitHub App. */
export function prPlansQuery(project: string): { query: "pr_plans" } & PrPlansQuery {
  return { query: "pr_plans", project };
}

/** A pull request's PR Environments, where each one's merge lands, and its GitHub check. */
export function pullRequestQuery(pullRequest: PullRequestRef): { query: "pull_request" } & PullRequestQuery {
  return { query: "pull_request", repository_id: pullRequest.repository_id, number: pullRequest.number };
}

/** What a PR Environment's Save would hold for one Destination, going live with the merge. */
export function atMergeQuery(from: EnvironmentRef, into: EnvironmentRef): { query: "move" } & MoveQuery {
  return { query: "move", move: "save", from, into, when: "at_merge" };
}

/** A plan in words: "On · from staging · db copied · then php artisan migrate". */
export function planSummary(plan: PrPlan) {
  if (!plan.enabled) return "Off";
  if (!plan.start_from) return "On · pick an environment to start from";
  const parts = ["On", `from ${plan.start_from}`];
  if (plan.copy.length) parts.push(`${listNames(plan.copy)} copied`);
  if (plan.setup.length) parts.push(`then ${plan.setup.map((setup) => setup.command).join(", ")}`);
  return parts.join(" · ");
}

/** One Destination of a PR Environment as its panel words it: what waits to be saved there, or what's saved. */
export type DestinationNews =
  | { kind: "save"; into: string; changes: number }
  | { kind: "saved"; into: string; changes: number; save: string }
  | { kind: "stale"; into: string; changes: number; save: string };

/**
 * A PR Environment's news per Destination, from its pull request's view: a standing Conditional Sync reads as saved
 * (Undo), one the PR Environment or target branch moved past as stale (save again), and changes nobody saved as to save.
 */
export function destinationNews(view: PullRequestView, environment: string): DestinationNews[] {
  const destinations = view.environments.find((row) => row.environment.name === environment)?.destinations ?? [];
  return destinations.flatMap((destination): DestinationNews[] => {
    const { conditional_sync: save, name: into, changes } = destination;
    if (save?.standing) return [{ kind: "saved", into, changes: save.changes, save: save.id }];
    if (save) return [{ kind: "stale", into, changes: Math.max(changes, save.changes), save: save.id }];
    return changes > 0 ? [{ kind: "save", into, changes }] : [];
  });
}

/** "2 changes go live when #142 merges". */
export const goLiveWhen = (changes: number, number: number) => `${plural(changes, "change")} · go live when #${number} merges`;

/**
 * Hints, a merged pull request's or a Parent's values, split by where Details shows them: beside the change to deploy
 * at the same path, else after the changes (a hint whose row nothing stages, like a variable edited here since).
 */
export function hintNotes<Hint extends Pick<PullRequestHint, "row">>(hints: readonly Hint[], paths: ReadonlySet<string>) {
  return {
    at: (path: string) => hints.filter((hint) => hint.row === path),
    rest: hints.filter((hint) => !paths.has(hint.row)),
  };
}
