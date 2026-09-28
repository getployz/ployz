import type { ConditionalSaveRow, PrShutdown } from "#/modules/pr-environments/tables";
import { plural } from "./branch-plan";
import type { BranchReviewView } from "./use-branch-review";

type Landing = BranchReviewView["goesTo"][number];

/** One thing between a Branch and where it saves. */
export type BranchNews =
  | { kind: "shutdown_failed" }
  /** `landing` is a PR Environment's Destination; null saves into the Parent. */
  | { kind: "save"; landing: Landing | null; count: number }
  | { kind: "update"; count: number }
  | { kind: "shutting_down" }
  | { kind: "off" }
  | { kind: "closing"; days: number }
  | { kind: "saved"; landing: Landing; saved: ConditionalSaveRow }
  | { kind: "up_to_date" };

/**
 * What's between a Branch and where it saves, most pressing first. The Branch button says the first; the Branch's panel
 * leads with it. A closed pull request takes no more saves.
 */
export function branchNews(
  review: Pick<BranchReviewView, "pullRequest" | "goesTo" | "save" | "updates">,
  shutdown: PrShutdown | null,
  closesIn: number | null,
): [BranchNews, ...BranchNews[]] {
  const landings = review.pullRequest?.closed === false ? review.goesTo : [];
  const news: BranchNews[] = [
    ...(shutdown === "failed" ? [{ kind: "shutdown_failed" } as const] : []),
    ...(review.pullRequest
      ? landings.flatMap((landing) => !landing.saved && landing.rows.length ? [{ kind: "save", landing, count: landing.rows.length } as const] : [])
      : review.save.length ? [{ kind: "save", landing: null, count: review.save.length } as const] : []),
    ...(review.updates ? [{ kind: "update", count: review.updates } as const] : []),
    ...(shutdown === "running" ? [{ kind: "shutting_down" } as const] : shutdown === "off" ? [{ kind: "off" } as const] : []),
    ...(closesIn !== null ? [{ kind: "closing", days: closesIn } as const] : []),
    ...landings.flatMap((landing) => landing.saved ? [{ kind: "saved", landing, saved: landing.saved } as const] : []),
  ];
  const [first = { kind: "up_to_date" }, ...rest] = news;
  return [first, ...rest];
}

/** The Branch button's words: the first news, with changes to save counted over every Destination. */
export function newsLabel([first, ...rest]: [BranchNews, ...BranchNews[]]) {
  switch (first.kind) {
    case "shutdown_failed": return "Shutdown failed";
    case "save": return `${rest.reduce((sum, news) => sum + (news.kind === "save" ? news.count : 0), first.count)} to save`;
    case "update": return plural(first.count, "update");
    case "shutting_down": return "Shutting down";
    case "off": return "Off";
    case "closing": return `Closes in ${plural(first.days, "day")}`;
    case "saved": return "Saved";
    case "up_to_date": return "Up to date";
  }
}
