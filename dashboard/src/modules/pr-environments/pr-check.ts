import { listNames, plural } from "#/modules/branches/branch-plan";

/** The check Ployz posts on a PR Environment's pull request. It never blocks a deploy; GitHub may require it to merge. */
export const PR_CHECK_NAME = "Ployz · ready to merge";

/** One Destination of a PR Environment: how many changes go there now, and its Conditional Save there, if any. */
export type PrCheckDestination = {
  name: string;
  changes: number;
  save: { standing: boolean; changes: number } | null;
};

export type PrCheck = { passing: boolean; reason: string };

const sum = (list: number[]) => list.reduce((total, n) => total + n, 0);

/**
 * Whether a PR Environment's settings are ready for its pull request to merge: every Destination with changes has a
 * standing Conditional Save. Browser and server both call it, so the bar, the review and GitHub say the same thing.
 */
export function prCheck(destinations: PrCheckDestination[], targetBranch: string): PrCheck {
  if (!destinations.length) return { passing: true, reason: `No environment deploys ${targetBranch}` };
  const waiting = destinations.filter((destination) => destination.changes > 0 || destination.save?.standing);
  if (!waiting.length) return { passing: true, reason: `No changes for ${listNames(destinations.map((d) => d.name))}` };
  if (waiting.some((destination) => destination.save && !destination.save.standing)) {
    return { passing: false, reason: "Changed since saved · save again" };
  }
  const unsaved = waiting.filter((destination) => !destination.save);
  if (unsaved.length) return { passing: false, reason: `${plural(sum(unsaved.map((d) => d.changes)), "change")} to save in Ployz` };
  const saved = sum(waiting.map((d) => d.save?.changes ?? 0));
  return { passing: true, reason: `${plural(saved, "change")} go${saved === 1 ? "es" : ""} live with this PR` };
}
