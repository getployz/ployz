import { listNames } from "#/modules/branches/branch-plan";

/** The check Ployz posts on a PR Environment's pull request. It never blocks a deploy; GitHub may require it to merge. */
export const PR_CHECK_NAME = "Ployz · ready to merge";

/** One Destination of a PR Environment: how many changes go there now, and its Conditional Save if any. */
export type PrCheckDestination = {
  name: string;
  changes: number;
  approval: { standing: boolean; changes: number; missing: string[]; approvedBy: string | null } | null;
};

export type PrCheck = { passing: boolean; reason: string };

const plural = (n: number, noun: string) => `${n} ${noun}${n === 1 ? "" : "s"}`;
const sum = (list: number[]) => list.reduce((total, n) => total + n, 0);

/**
 * Whether a PR Environment's settings are sorted for merging: every Destination with changes has a standing approval
 * with every value it needs. Browser and server both call it, so the review, the bar and GitHub say the same thing.
 */
export function prCheck(destinations: PrCheckDestination[]): PrCheck {
  if (!destinations.length) return { passing: true, reason: "Nothing here deploys its target branch" };
  const held = destinations.filter((destination) => destination.changes > 0 || destination.approval?.standing);
  if (!held.length) return { passing: true, reason: `Nothing here that ${listNames(destinations.map((d) => d.name))} doesn’t have` };
  const unapproved = held.filter((destination) => !destination.approval);
  if (unapproved.length) {
    return {
      passing: false,
      reason: `Review and approve ${plural(sum(unapproved.map((d) => d.changes)), "change")} for ${listNames(unapproved.map((d) => d.name))}`,
    };
  }
  const approvals = held.flatMap((destination) => destination.approval ? [{ ...destination.approval, name: destination.name }] : []);
  if (approvals.some((approval) => !approval.standing)) return { passing: false, reason: "Changed since it was approved · review it again" };
  const lacking = approvals.filter((approval) => approval.missing.length);
  if (lacking.length) {
    const keys = [...new Set(lacking.flatMap((approval) => approval.missing))];
    return { passing: false, reason: `${keys.join(", ")} need${keys.length === 1 ? "s" : ""} a value for ${listNames(lacking.map((a) => a.name))}` };
  }
  const approvers = [...new Set(approvals.flatMap((approval) => approval.approvedBy ?? []))];
  return {
    passing: true,
    reason: `${plural(sum(approvals.map((a) => a.changes)), "change")} approved for ${listNames(approvals.map((a) => a.name))}${approvers.length ? ` by ${listNames(approvers)}` : ""}`,
  };
}
