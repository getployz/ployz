import { Link, useParams } from "@tanstack/react-router";
import { GitBranchIcon } from "lucide-react";
import { buttonVariants } from "#/components/ui/button-variants";
import { cn } from "#/lib/utils";
import { useIdleClose, useShutdown } from "#/modules/branches/branch.collection";
import { plural } from "#/modules/branches/branch-plan";
import { useBranchReview } from "#/modules/branches/use-branch-review";
import type { PrShutdown } from "#/modules/pr-environments/tables";
import { ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/** The Branch button's words: the first that applies. */
export function branchStatus({ changes, updates, saved, shutdown, closesIn }: {
  changes: number;
  updates: number;
  /** A PR Environment's saved changes, waiting for the merge. */
  saved: boolean;
  shutdown: PrShutdown | null;
  /** Days until it closes itself, once that's close. */
  closesIn: number | null;
}) {
  if (shutdown === "failed") return "Shutdown failed";
  if (changes) return `${changes} to save`;
  if (updates) return plural(updates, "update");
  if (shutdown) return shutdown === "running" ? "Shutting down" : "Off";
  if (closesIn !== null) return `Closes in ${plural(closesIn, "day")}`;
  return saved ? "Saved" : "Up to date";
}

/**
 * A Branch's button at the canvas's top right: what's between it and its Parent, in a few words, opening its panel.
 * Save and Update live there, not in the bottom bar: they bring changes from one Environment to another, where they
 * land as changes to deploy.
 */
export function BranchButton({ environmentId }: { environmentId: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const review = useBranchReview(params.organizationSlug, environmentId);
  const shutdown = useShutdown(params.organizationSlug, environmentId);
  const idle = useIdleClose(params.organizationSlug, environmentId);
  if (!review) return null;
  // A closed pull request takes no more saves.
  const open = !review.pullRequest?.closed;
  const status = branchStatus({
    changes: open ? review.changes : 0,
    updates: review.updates,
    saved: open && review.pullRequest !== null && review.goesTo.some((landing) => landing.saved),
    shutdown,
    closesIn: idle.kind === "warn" ? idle.daysLeft : null,
  });
  return (
    <Link to={ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO} params={params} aria-label={`Branch: ${status}`}
      className={cn(buttonVariants({ variant: "outline" }), "pointer-events-auto")}>
      <GitBranchIcon data-icon="inline-start" />{status}
    </Link>
  );
}
