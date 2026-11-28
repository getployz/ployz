import { useState } from "react";
import { Link, useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import { DashboardPage } from "#/components/dashboard-page";
import { Button } from "#/components/ui/button";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import { Spinner } from "#/components/ui/spinner";
import { PREVIEW_ROUTE_TO, PREVIEWS_ROUTE_TO, type PreviewRouteParams } from "#/lib/environment-routes";
import type { PreviewsView } from "#/modules/previews/previews.contract";
import { usePreviews } from "#/modules/previews/previews.queries";
import { usePreviewWrites, writeFailure } from "#/modules/previews/preview-writes";
import { CloseOneList, PreviewsRefused, RaiseLimit, upRows } from "./preview-parts";

/**
 * Close a preview: the live previews by last push, each with Close, and Raise the limit. Opened for pull request
 * `wants`, it deploys that one once there's room.
 */
export function ClosePreviewPage({ params, repository, wants }: { params: PreviewRouteParams; repository: string; wants: number | null }) {
  const result = usePreviews(params.organizationSlug, { project: params.projectSlug, repository, number: null });
  return (
    <DashboardPage width="content" title="Close a preview">
      {!result.ok ? <PreviewsRefused message={result.refusal.message} /> : <Body view={result.value} params={params} wants={wants} />}
    </DashboardPage>
  );
}

function Body({ view, params, wants }: { view: PreviewsView; params: PreviewRouteParams; wants: number | null }) {
  const full = view.live >= view.limit;
  const want = wants === null ? undefined : view.pull_requests.find((row) => row.number === wants && row.open && row.preview.state === "not_deployed");
  if (upRows(view).length === 0) {
    return (
      <Empty variant="placeholder">
        <EmptyHeader>
          <EmptyTitle>No live previews</EmptyTitle>
          <EmptyDescription>There's room for {view.limit} previews.</EmptyDescription>
        </EmptyHeader>
        <EmptyContent>
          {want ? <DeployWanted view={view} params={params} number={want.number} head={want.head} />
            : <Button variant="outline" render={<Link to={PREVIEWS_ROUTE_TO} params={params} />}>Pull requests</Button>}
        </EmptyContent>
      </Empty>
    );
  }
  return (
    <section aria-labelledby="live-count" className="flex flex-col gap-3">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="flex flex-col gap-1">
          <h2 id="live-count" className="text-base font-semibold">{view.live} of {view.limit} previews are live</h2>
          <p className="text-sm text-muted-foreground">
            {!full ? `There's room for ${view.limit - view.live} more.`
              : want ? `Close one to deploy #${want.number} ${want.title}, or raise the limit.`
              : "Close one to make room, or raise the limit."}
          </p>
        </div>
        {want && !full ? <DeployWanted view={view} params={params} number={want.number} head={want.head} /> : <RaiseLimit params={params} />}
      </div>
      <CloseOneList view={view} params={params} project={params.projectSlug} organizationSlug={params.organizationSlug} />
    </section>
  );
}

/** Deploys the pull request Close a preview was opened for, then opens its page. */
function DeployWanted({ view, params, number, head }: { view: PreviewsView; params: PreviewRouteParams; number: number; head: string | null }) {
  const writes = usePreviewWrites(params.organizationSlug);
  const navigate = useNavigate();
  const [deploying, setDeploying] = useState(false);
  if (head === null) return null;
  const deploy = async () => {
    setDeploying(true);
    try {
      await writes.deploy({ repository_id: view.repository_id, number, head });
      toast.success(`Deploying #${number}`);
      void navigate({ to: PREVIEW_ROUTE_TO, params: { ...params, number: String(number) } });
    } catch (error) {
      toast.error(writeFailure(error));
    } finally {
      setDeploying(false);
    }
  };
  return (
    <Button disabled={deploying} onClick={() => void deploy()}>
      {deploying ? <Spinner data-icon="inline-start" /> : null}
      Deploy #{number}
    </Button>
  );
}
