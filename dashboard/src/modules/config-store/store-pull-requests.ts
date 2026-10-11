import type {
  EnvironmentRef, MergeRow, NamedRow, OnMerge, OnMergeQuery, PrPlan, PrPlansQuery, PullRequestQuery, PullRequestRef,
  PullRequestView, RowId,
} from "@ployz/sdk";
import { listNames } from "#/lib/plural";

/** A Project's PR plans: one per repository its Services deploy from through the GitHub App. */
export function prPlansQuery(project: string): { query: "pr_plans" } & PrPlansQuery {
  return { query: "pr_plans", project };
}

/** A pull request's PR Environments, where each one's merge lands, and its GitHub check. */
export function pullRequestQuery(pullRequest: PullRequestRef): { query: "pull_request" } & PullRequestQuery {
  return { query: "pull_request", repository_id: pullRequest.repository_id, number: pullRequest.number };
}

/** What a PR Environment's merge lands, one list per Destination. */
export function onMergeQuery(environment: EnvironmentRef): { query: "on_merge" } & OnMergeQuery {
  return { query: "on_merge", environment };
}

/** A row the merge lands: as the PR Environment has it, or as edited. */
export const rides = (row: MergeRow) => row.ride === "rides" || row.ride === "edited";

/** The On merge list: each Destination's rows the PR Environment changed, but not those its Destination already has. */
export function onMergeLists(view: OnMerge) {
  return view.destinations.map((into) => ({ name: into.name, rows: into.rows.filter((row) => row.ride !== "production_keeps") }));
}

/** How many changes the merge lands, every Destination counted. */
export function onMergeCount(view: OnMerge) {
  return view.destinations.reduce((count, into) => count + into.rows.filter(rides).length, 0);
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

/** The check Ployz posts on a PR Environment's pull request: what its merge lands. Never failing, so it never blocks a merge. */
export const PR_CHECK_NAME = "Ployz · ready to merge";

/**
 * The check run the Store's views of one pull request say to post, if any. Each Organization's Store says what the
 * merge lands (or, once merged, what didn't); several Organizations' words join into the one check, neutral if any is.
 */
export function storePrCheckRun(views: readonly { slug: string; view: PullRequestView }[], appUrl: string) {
  const saying = views.flatMap(({ slug, view }) =>
    view.check && view.pull_request ? [{ slug, view, check: view.check, head: view.pull_request.head }] : []);
  const [first] = saying;
  if (!first) return null;
  const environment = first.view.environments[0]?.environment;
  const details = new URL(`cloud/${first.slug}/${environment?.project ?? ""}/${environment?.name ?? ""}`, appUrl);
  details.searchParams.set("panel", "on-merge");
  return {
    headSha: first.head,
    name: PR_CHECK_NAME,
    conclusion: saying.some(({ check }) => check.conclusion === "neutral") ? "neutral" as const : "success" as const,
    detailsUrl: details.href,
    title: saying.map(({ check }) => check.title).join("; "),
    summary: `${saying.map(({ check }) => check.summary).join("\n\n")}\n\n[View in Ployz](${details.href})`,
  };
}
