import { useContext, type ReactNode } from "react";
import { useLoaderData, useParams } from "@tanstack/react-router";
import { CircleCheckIcon, GitPullRequestIcon, TriangleAlertIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { listNames, plural } from "#/modules/branches/branch-plan";
import { useBranchReview, type BranchReviewView, type PullRequest } from "#/modules/branches/use-branch-review";
import { PR_CHECK_NAME, type PrCheck } from "#/modules/pr-environments/pr-check";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { StagedReviewSlot } from "../canvas/BottomBar";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { DifferSection } from "./DifferSection";
import { GoesToSection } from "./GoesToSection";
import { HeldChanges } from "./HeldChanges";
import { MergeSection } from "./MergeSection";
import { UpdateSection } from "./UpdateSection";

/**
 * A Branch's review page, the whole relationship with its Parent in four sections: what's staged here, what would merge
 * into the Destination (the Parent), what's new in the Parent, and what's meant to differ. A Kept Branch gets the same page.
 * A PR Environment never Merges: its page shows its pull request's code, then what goes to each of its Destinations.
 */
export function BranchReviewPanel() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const name = useEnvironmentDocument(params.organizationSlug, environmentId)?.name ?? params.environmentSlug;
  const review = useBranchReview(params.organizationSlug, environmentId);
  const { setSlot } = useContext(StagedReviewSlot);
  const pr = review?.pullRequest ?? null;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <span className="font-medium">{pr ? `What #${pr.number} changes` : `Review ${name}`}</span>
        {review ? <p className="truncate text-sm text-muted-foreground">{name} ⑂ {review.parent.name}{landsIn(review)}</p> : null}
      </CanvasInspectorHeader>
      {review ? (
        <div className="flex min-h-0 flex-1 flex-col gap-6 overflow-y-auto p-4">
          <ReviewSection title={`Not deployed to ${name} yet`}>
            {/* The bottom bar owns the change actions and renders the staged-changes review here. */}
            <div ref={setSlot} />
          </ReviewSection>
          <HeldChanges environmentId={environmentId} />
          {pr ? (
            <>
              <CodeSection review={review} pr={pr} />
              {review.goesTo.map((landing) => (
                <GoesToSection key={landing.destination.id} review={review} pr={pr} landing={landing} name={name} environmentId={environmentId} />
              ))}
              {review.check && !pr.closed ? <CheckSection check={review.check} /> : null}
            </>
          ) : <MergeSection review={review} branch={{ id: environmentId, name }} />}
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

/** " · lands in production" when changes land somewhere other than the Parent. */
function landsIn(review: BranchReviewView) {
  const names = review.goesTo.map((landing) => landing.destination.name);
  return names.length && !(names.length === 1 && names[0] === review.parent.name) ? ` · lands in ${listNames(names)}` : "";
}

/** The pull request, and where merging it on GitHub deploys. */
function CodeSection({ review, pr }: { review: BranchReviewView; pr: PullRequest }) {
  const names = review.goesTo.map((landing) => landing.destination.name);
  return (
    <ReviewSection title="Code" help={names.length ? `Merging deploys ${listNames(names)}.` : "Merging moves no settings."}>
      <ItemGroup>
        <Item variant="outline" size="sm">
          <ItemMedia variant="icon"><GitPullRequestIcon /></ItemMedia>
          <ItemContent className="min-w-0">
            <ItemTitle className="flex-wrap">#{pr.number} {pr.title}</ItemTitle>
            <ItemDescription className="wrap-anywhere">
              <span className="font-mono">{pr.headBranch}</span> → <span className="font-mono">{pr.targetBranch}</span> · {pr.author} · {plural(pr.commits, "commit")}
            </ItemDescription>
          </ItemContent>
          <ItemActions><ItemDescription>{pr.closed ? "Closed on GitHub" : "Merges on GitHub"}</ItemDescription></ItemActions>
        </Item>
      </ItemGroup>
    </ReviewSection>
  );
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
