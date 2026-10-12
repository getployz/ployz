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
/** A riding row the merge lands; a removal production still uses stays. */
const lands = (row: MergeRow) => rides(row) && !row.refused_by?.length;

/**
 * The On merge list: each Destination's rows the PR Environment changed, but not those its Destination already has.
 * A removed Volume stays listed as Kept, so the list says where its data went.
 */
export function onMergeLists(view: OnMerge) {
  return view.destinations.map((into) => ({ name: into.name, rows: into.rows.filter((row) => row.ride !== "production_keeps") }));
}

/** How many changes the merge lands, every Destination counted. */
export function onMergeCount(view: OnMerge) {
  return view.destinations.reduce((count, into) => count + into.rows.filter(lands).length, 0);
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
  const into = first.check.merged_into;
  // Once merged, the PR Environment may be gone: link the Destination's History.
  const details = into?.project && into.environment
    ? new URL(`cloud/${first.slug}/${into.project}/${into.environment}/history`, appUrl)
    : new URL(`cloud/${first.slug}/${environment?.project ?? ""}/${environment?.name ?? ""}`, appUrl);
  if (!into) details.searchParams.set("panel", "on-merge");
  return {
    headSha: first.head,
    name: PR_CHECK_NAME,
    conclusion: saying.some(({ check }) => check.conclusion === "neutral") ? "neutral" as const : "success" as const,
    detailsUrl: details.href,
    title: saying.map(({ check }) => check.title).join("; "),
    summary: fitSummary(saying.map(({ check }) => check.summary).join("\n\n"), `\n\n[View in Ployz](${details.href})`),
  };
}

/** The most a check run's summary may hold, in UTF-8 bytes: GitHub refuses a longer one. */
export const CHECK_SUMMARY_LIMIT = 65_535;

const utf8 = new TextEncoder();
const bytes = (text: string) => utf8.encode(text).length;

/** `body` then `link`, within GitHub's limit: whole lines of `body` while they fit, then how many more there are. */
function fitSummary(body: string, link: string) {
  if (bytes(body) + bytes(link) <= CHECK_SUMMARY_LIMIT) return body + link;
  // Room for the "…and N more lines" note.
  const room = CHECK_SUMMARY_LIMIT - bytes(link) - 32;
  const lines = body.split("\n");
  const kept: string[] = [];
  let length = 0;
  for (const line of lines) {
    if (length + bytes(line) + 1 > room) break;
    kept.push(line);
    length += bytes(line) + 1;
  }
  // A single line too long to keep whole: as many characters as fit.
  const head = kept.length ? kept.join("\n") : body.slice(0, Math.floor(room / 4));
  return `${head}\n…and ${lines.length - kept.length} more lines${link}`;
}
