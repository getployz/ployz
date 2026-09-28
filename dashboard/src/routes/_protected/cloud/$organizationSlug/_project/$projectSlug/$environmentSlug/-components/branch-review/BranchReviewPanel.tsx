import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import type { BranchRow } from "#/collections/collections";
import { useIdleClose } from "#/modules/branches/branch.collection";
import { branchNews } from "#/modules/branches/branch-news";
import { useBranchReview, type BranchReviewView } from "#/modules/branches/use-branch-review";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { useBranchLifecycle } from "../branch-lifecycle";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { BranchNewsList } from "./BranchNews";

type Params = { organizationSlug: string; projectSlug: string; environmentSlug: string };

/**
 * A Branch's panel, opened by the Branch button at the canvas's top right. It does one job, moving changes between the
 * Branch and its Parent, leading with the thing to do next as the button says it. Keep, Shut down and Close wait in ⋮.
 */
export function BranchReviewPanel() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const name = useEnvironmentDocument(params.organizationSlug, environmentId)?.name ?? params.environmentSlug;
  const review = useBranchReview(params.organizationSlug, environmentId);
  const { environments, branches } = useWorkspace(params.organizationSlug);
  const branch = branches.find((row) => row.environmentId === environmentId);
  const parent = environments.find((row) => row.id === branch?.parentEnvironmentId);
  return review && branch && parent ? (
    <BranchPanel params={params} environmentId={environmentId} name={name} review={review} branch={branch} parent={parent} />
  ) : (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}><span className="font-medium">{name}</span></CanvasInspectorHeader>
      <Empty><EmptyDescription>Only branches have this panel.</EmptyDescription></Empty>
    </div>
  );
}

function BranchPanel({ params, environmentId, name, review, branch, parent }: {
  params: Params; environmentId: string; name: string; review: BranchReviewView; branch: BranchRow;
  parent: { name: string; namespace: string };
}) {
  const idle = useIdleClose(params.organizationSlug, environmentId);
  const lifecycle = useBranchLifecycle({ ...params, branch, name, parent });
  const pr = branch.pullRequest;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params} actions={lifecycle.menu}>
        <span className="font-medium">{name}</span>
        <p className="truncate text-sm text-muted-foreground">
          Branch of <Link to={ENVIRONMENT_INDEX_ROUTE_TO} params={{ ...params, environmentSlug: parent.namespace }}
            className="underline underline-offset-4">{parent.name}</Link>{pr ? ` · PR #${pr.number} · ${pr.title}` : ""}
        </p>
      </CanvasInspectorHeader>
      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-4">
        {lifecycle.dialogs}
        <BranchNewsList news={branchNews(review, pr?.shutdown ?? null, idle.kind === "warn" ? idle.daysLeft : null)}
          review={review} environmentId={environmentId} name={name} />
      </div>
    </div>
  );
}
