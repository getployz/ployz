import type { ReactNode } from "react";
import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import { CircleCheckIcon, TriangleAlertIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { listNames } from "#/modules/branches/branch-plan";
import { useBranchReview, type BranchReviewView } from "#/modules/branches/use-branch-review";
import { PR_CHECK_NAME, type PrCheck } from "#/modules/pr-environments/pr-check";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { getPrEnvironmentPlansCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { defaultPrEnvironmentPlan } from "#/modules/pr-environments/repositories";
import { BranchSections } from "../branch-settings-section";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { DifferSection } from "./DifferSection";
import { UpdateSection } from "./UpdateSection";

/**
 * A Branch's Manage panel, its one home: where it came from, its check on GitHub, what's new in its Parent, what stays
 * different, and Keep, Shut down and Close. Save stays in the bottom bar's sheet, beside this panel's Manage button.
 */
export function BranchReviewPanel() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const name = useEnvironmentDocument(params.organizationSlug, environmentId)?.name ?? params.environmentSlug;
  const review = useBranchReview(params.organizationSlug, environmentId);
  const { environments, branches } = useWorkspace(params.organizationSlug);
  const branch = branches.find((row) => row.environmentId === environmentId);
  const parent = environments.find((row) => row.id === branch?.parentEnvironmentId);
  const { data: plans } = useLiveSuspenseQuery(getPrEnvironmentPlansCollection(params.organizationSlug, useCollectionScope()));
  const plan = plans.find((row) => row.projectId === branch?.projectId && row.repositoryId === branch.pullRequest?.repositoryId);
  const pr = review?.pullRequest ?? null;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <span className="font-medium">{name}</span>
        {parent ? <p className="truncate text-sm text-muted-foreground">
          Branch of <Link to={ENVIRONMENT_INDEX_ROUTE_TO} params={{ ...params, environmentSlug: parent.namespace }}
            className="underline underline-offset-4">{parent.name}</Link>{review ? landsIn(review) : ""}
        </p> : null}
      </CanvasInspectorHeader>
      {review && branch && parent ? (
        <div className="flex min-h-0 flex-1 flex-col gap-8 overflow-y-auto p-4">
          {branch.pullRequest ? <p className="text-sm text-muted-foreground">
            {`PR environment for #${branch.pullRequest.number} · ${branch.pullRequest.title} · ${(plan ?? defaultPrEnvironmentPlan).removeOnClose
              ? "closes with the pull request" : "stays 7 days after its last deploy"}`}
          </p> : null}
          {review.check && pr && !pr.closed ? <CheckSection check={review.check} /> : null}
          <UpdateSection review={review} environmentId={environmentId} />
          <DifferSection review={review} />
          <BranchSections organizationSlug={params.organizationSlug} projectSlug={params.projectSlug} environmentSlug={params.environmentSlug}
            branch={branch} name={name} parent={parent} />
        </div>
      ) : (
        <Empty><EmptyDescription>Only branches have this panel.</EmptyDescription></Empty>
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
