import type {
  NamedRow, PrPlan, PrPlansQuery, PullRequestQuery, PullRequestRef, RowId,
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

/** A plan in words: "On · from staging · db copied · then php artisan migrate". */
export function planSummary(plan: PrPlan) {
  if (!plan.enabled) return "Off";
  if (!plan.start_from) return "On · pick an environment to start from";
  const parts = ["On", `from ${plan.start_from}`];
  if (plan.copy.length) parts.push(`${listNames(plan.copy)} copied`);
  if (plan.setup.length) parts.push(`then ${plan.setup.map((setup) => setup.command).join(", ")}`);
  return parts.join(" · ");
}

/** "2 changes go live when #142 merges". */
export const goLiveWhen = (changes: number, number: number) => `${plural(changes, "change")} · go live when #${number} merges`;

/**
 * A Parent's values as hints, split by where Details shows them: beside the change to deploy
 * in the same row, else after the changes (a hint whose row nothing stages, like a variable edited here since).
 */
export function hintNotes<Hint extends NamedRow>(hints: readonly Hint[], rows: ReadonlySet<RowId>) {
  return {
    at: (row: RowId | null) => hints.filter((hint) => hint.row === row),
    rest: hints.filter((hint) => !rows.has(hint.row)),
  };
}
