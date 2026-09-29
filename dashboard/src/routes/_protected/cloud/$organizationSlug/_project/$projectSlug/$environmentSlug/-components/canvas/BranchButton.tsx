import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import { GitBranchIcon } from "lucide-react";
import { buttonVariants } from "#/components/ui/button-variants";
import { cn } from "#/lib/utils";
import { useIdleClose, useShutdown } from "#/modules/branches/branch.collection";
import { branchNews, newsLabel } from "#/modules/branches/branch-news";
import { plural } from "#/modules/branches/branch-plan";
import { storeEnabled } from "#/modules/config-store/store.contract";
import { branchQuery, saveQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useBranchReview } from "#/modules/branches/use-branch-review";
import { ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/**
 * A Branch's button at the canvas's top right: its first news in a few words, opening its panel. Save and Update live
 * there, not in the bottom bar: they bring changes from one Environment to another, where they land as changes to deploy.
 */
export function BranchButton({ environmentId }: { environmentId: string }) {
  return storeEnabled ? <StoreBranchButton /> : <CloudBranchButton environmentId={environmentId} />;
}

/** Over the Config Store: changes to save first, then updates from the Parent. */
function StoreBranchButton() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const branch = useStoreView(params.organizationSlug, branchQuery(store));
  const save = useStoreView(params.organizationSlug, saveQuery(store));
  if (!branch.ok) return null;
  const toSave = save.ok ? save.value.rows.length : 0;
  const status = toSave ? `${toSave} to save` : branch.value.update.length ? plural(branch.value.update.length, "update") : "Up to date";
  return (
    <Link to={ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO} params={params} aria-label={`Branch: ${status}`}
      className={cn(buttonVariants({ variant: "outline" }), "pointer-events-auto")}>
      <GitBranchIcon data-icon="inline-start" />{status}
    </Link>
  );
}

function CloudBranchButton({ environmentId }: { environmentId: string }) {
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
