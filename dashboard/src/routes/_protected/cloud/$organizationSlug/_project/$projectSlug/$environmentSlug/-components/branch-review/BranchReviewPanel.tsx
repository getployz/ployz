import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Separator } from "#/components/ui/separator";
import { useIdleClose } from "#/modules/branches/branch.collection";
import { branchNews } from "#/modules/branches/branch-news";
import { useBranchReview } from "#/modules/branches/use-branch-review";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { BranchSections } from "../branch-settings-section";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { BranchNewsList } from "./BranchNews";

/**
 * A Branch's panel, its one home, opened by the Branch button at the canvas's top right. It leads with the one thing to
 * do next, as the button says it; everything else is a line; Keep, Shut down and Close come last.
 */
export function BranchReviewPanel() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const name = useEnvironmentDocument(params.organizationSlug, environmentId)?.name ?? params.environmentSlug;
  const review = useBranchReview(params.organizationSlug, environmentId);
  const idle = useIdleClose(params.organizationSlug, environmentId);
  const { environments, branches } = useWorkspace(params.organizationSlug);
  const branch = branches.find((row) => row.environmentId === environmentId);
  const parent = environments.find((row) => row.id === branch?.parentEnvironmentId);
  const pr = branch?.pullRequest ?? null;
  const news = review && branchNews(review, pr?.shutdown ?? null, idle.kind === "warn" ? idle.daysLeft : null);
  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <span className="font-medium">{name}</span>
        {parent ? <p className="truncate text-sm text-muted-foreground">
          Branch of <Link to={ENVIRONMENT_INDEX_ROUTE_TO} params={{ ...params, environmentSlug: parent.namespace }}
            className="underline underline-offset-4">{parent.name}</Link>{pr ? ` · PR #${pr.number} · ${pr.title}` : ""}
        </p> : null}
      </CanvasInspectorHeader>
      {review && news && branch && parent ? (
        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-4">
          <BranchNewsList news={news} review={review} environmentId={environmentId} name={name} />
          <Separator />
          <BranchSections organizationSlug={params.organizationSlug} projectSlug={params.projectSlug} environmentSlug={params.environmentSlug}
            branch={branch} name={name} parent={parent} closing={news.some((item) => item.kind === "closing")} />
        </div>
      ) : (
        <Empty><EmptyDescription>Only branches have this panel.</EmptyDescription></Empty>
      )}
    </div>
  );
}
