import { useContext, type ReactNode } from "react";
import { useLoaderData, useParams } from "@tanstack/react-router";
import { Badge } from "#/components/ui/badge";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { useBranchReview } from "#/modules/branches/use-branch-review";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { StagedReviewSlot } from "../canvas/BottomBar";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { DifferSection } from "./DifferSection";
import { MergeSection } from "./MergeSection";
import { UpdateSection } from "./UpdateSection";

/**
 * A Branch's review page, the whole relationship with its Parent in four sections: what's staged here, what would merge
 * into the Destination (the Parent), what's new in the Parent, and what's meant to differ. A Kept Branch gets the same page.
 */
export function BranchReviewPanel() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const name = useEnvironmentDocument(params.organizationSlug, environmentId)?.name ?? params.environmentSlug;
  const review = useBranchReview(params.organizationSlug, environmentId);
  const { setSlot } = useContext(StagedReviewSlot);
  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <span className="font-medium">Review {name}</span>
        {review ? <p className="truncate text-sm text-muted-foreground">{name} ⑂ {review.parent.name}</p> : null}
      </CanvasInspectorHeader>
      {review ? (
        <div className="flex min-h-0 flex-1 flex-col gap-6 overflow-y-auto p-4">
          <ReviewSection title={`Not deployed to ${name} yet`}>
            {/* The bottom bar owns the change actions and renders the staged-changes review here. */}
            <div ref={setSlot} />
          </ReviewSection>
          <MergeSection review={review} />
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
