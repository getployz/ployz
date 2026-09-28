import { Link, useParams } from "@tanstack/react-router";
import { GitBranchIcon } from "lucide-react";
import { buttonVariants } from "#/components/ui/button-variants";
import { cn } from "#/lib/utils";
import { useIdleClose, useShutdown } from "#/modules/branches/branch.collection";
import { branchNews, newsLabel } from "#/modules/branches/branch-news";
import { useBranchReview } from "#/modules/branches/use-branch-review";
import { ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/**
 * A Branch's button at the canvas's top right: its first news in a few words, opening its panel. Save and Update live
 * there, not in the bottom bar: they bring changes from one Environment to another, where they land as changes to deploy.
 */
export function BranchButton({ environmentId }: { environmentId: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const review = useBranchReview(params.organizationSlug, environmentId);
  const shutdown = useShutdown(params.organizationSlug, environmentId);
  const idle = useIdleClose(params.organizationSlug, environmentId);
  if (!review) return null;
  const status = newsLabel(branchNews(review, shutdown, idle.kind === "warn" ? idle.daysLeft : null));
  return (
    <Link to={ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO} params={params} aria-label={`Branch: ${status}`}
      className={cn(buttonVariants({ variant: "outline" }), "pointer-events-auto")}>
      <GitBranchIcon data-icon="inline-start" />{status}
    </Link>
  );
}
