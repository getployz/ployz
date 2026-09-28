import type { ReactNode } from "react";
import { useLoaderData, useParams } from "@tanstack/react-router";
import { CircleCheckIcon, TriangleAlertIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { listNames } from "#/modules/branches/branch-plan";
import { useBranchReview, type BranchReviewView } from "#/modules/branches/use-branch-review";
import { PR_CHECK_NAME, type PrCheck } from "#/modules/pr-environments/pr-check";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { DifferSection } from "./DifferSection";
import { UpdateSection } from "./UpdateSection";

/**
 * A Branch's review page: what's new in its Parent, and what stays different. Save lives in the bottom bar's sheet. A
 * Kept Branch gets the same page. A PR Environment's page also shows its check on GitHub.
 */
export function BranchReviewPanel() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const name = useEnvironmentDocument(params.organizationSlug, environmentId)?.name ?? params.environmentSlug;
  const review = useBranchReview(params.organizationSlug, environmentId);
  const pr = review?.pullRequest ?? null;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <span className="font-medium">Review {name}</span>
        {review ? <p className="truncate text-sm text-muted-foreground">{name} ⑂ {review.parent.name}{landsIn(review)}</p> : null}
      </CanvasInspectorHeader>
      {review ? (
        <div className="flex min-h-0 flex-1 flex-col gap-6 overflow-y-auto p-4">
          {review.check && pr && !pr.closed ? <CheckSection check={review.check} /> : null}
          <UpdateSection review={review} environmentId={environmentId} />
          <DifferSection review={review} />
        </div>
      ) : (
        <Empty><EmptyDescription>Only branches have a review page.</EmptyDescription></Empty>
      )}
    </div>
  );
}

export function ReviewSection({ title, count, help, children }: { title: string; count?: number; help?: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-3" aria-label={title}>
      <h3 className="flex items-center gap-2 font-medium">
        {title}{count ? <Badge variant="secondary" className="tabular-nums">{count}</Badge> : null}
      </h3>
      {children}
      {help ? <p className="text-sm text-muted-foreground">{help}</p> : null}
    </section>
  );
}

/** " · into production" when changes go somewhere other than the Parent. */
function landsIn(review: BranchReviewView) {
  const names = review.goesTo.map((landing) => landing.destination.name);
  return names.length && !(names.length === 1 && names[0] === review.parent.name) ? ` · into ${listNames(names)}` : "";
}

/** The check Ployz posts on the pull request, as GitHub shows it. */
function CheckSection({ check }: { check: PrCheck }) {
  return (
    <ReviewSection title="On GitHub" help="Make it required in GitHub to block merges.">
      <ItemGroup>
        <Item variant="outline" size="sm">
          <ItemMedia variant="icon">{check.passing ? <CircleCheckIcon className="text-success" /> : <TriangleAlertIcon className="text-warning" />}</ItemMedia>
          <ItemContent className="min-w-0">
            <ItemTitle>{PR_CHECK_NAME}</ItemTitle>
            <ItemDescription className="wrap-anywhere">{check.reason}</ItemDescription>
          </ItemContent>
          <ItemActions>
            <Badge variant={check.passing ? "success" : "warning"}>{check.passing ? "Passing" : "Action required"}</Badge>
          </ItemActions>
        </Item>
      </ItemGroup>
    </ReviewSection>
  );
}
