import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import { GitBranchIcon } from "lucide-react";
import { buttonVariants } from "#/components/ui/button-variants";
import { cn } from "#/lib/utils";
import { plural } from "#/lib/plural";
import { branchQuery, saveQuery, useStoreViews } from "#/modules/config-store/store-view.queries";
import { ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/**
 * A Branch's button at the canvas's top right: its first news in a few words, opening its panel. Save and Update live
 * there, not in the bottom bar: they bring changes from one Environment to another, where they land as changes to deploy.
 * Changes to save come first, then updates from the Parent.
 */
export function BranchButton() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const [branch, save] = useStoreViews(params.organizationSlug, [branchQuery(store), saveQuery(store)] as const);
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
