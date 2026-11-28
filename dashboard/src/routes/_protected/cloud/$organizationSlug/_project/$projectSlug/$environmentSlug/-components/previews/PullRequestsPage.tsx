import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { toast } from "sonner";
import { DashboardPage } from "#/components/dashboard-page";
import { RelativeTime } from "#/components/relative-time";
import { Button } from "#/components/ui/button";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "#/components/ui/table";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import { PREVIEW_CLOSE_ROUTE_TO, PREVIEW_ROUTE_TO, PREVIEW_SETTINGS_ROUTE_TO, type PreviewRouteParams } from "#/lib/environment-routes";
import { useOrganizationName } from "#/modules/organization/organization-state.queries";
import type { PreviewRow, PreviewsView } from "#/modules/previews/previews.contract";
import { usePreviews } from "#/modules/previews/previews.queries";
import { usePreviewWrites, writeFailure } from "#/modules/previews/preview-writes";
import { addressHref, authorLine, deployable, outcomeLine, PreviewsRefused, PreviewStatus } from "./preview-parts";

/** A repository's pull requests, each with its preview's one outcome line and the one action that changes it. */
export function PullRequestsPage({ params, repository }: { params: PreviewRouteParams; repository: string }) {
  const result = usePreviews(params.organizationSlug, { project: params.projectSlug, repository, number: null });
  const [filter, setFilter] = useState<"open" | "closed">("open");
  return (
    <DashboardPage width="content" title="Pull requests" actions={
      <Button variant="outline" render={<Link to={PREVIEW_SETTINGS_ROUTE_TO} params={params} />}>Preview settings</Button>
    }>
      {!result.ok ? <PreviewsRefused message={result.refusal.message} />
        : <Rows view={result.value} params={params} filter={filter} onFilter={setFilter} />}
    </DashboardPage>
  );
}

function Rows({ view, params, filter, onFilter }: {
  view: PreviewsView;
  params: PreviewRouteParams;
  filter: "open" | "closed";
  onFilter: (filter: "open" | "closed") => void;
}) {
  const organizationName = useOrganizationName(params.organizationSlug);
  if (view.settings === null) {
    return (
      <Empty variant="placeholder">
        <EmptyHeader>
          <EmptyTitle>Previews are off</EmptyTitle>
          <EmptyDescription>Deploy any pull request to {view.repository} as a copy of an environment, at its own address.</EmptyDescription>
        </EmptyHeader>
        <EmptyContent>
          <Button render={<Link to={PREVIEW_SETTINGS_ROUTE_TO} params={params} search={{ edit: true }} />}>Set up previews</Button>
        </EmptyContent>
      </Empty>
    );
  }
  const rows = view.pull_requests.filter((row) => row.open === (filter === "open"));
  const full = view.live >= view.limit;
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-3">
        <p className="min-w-0 flex-1 text-sm text-muted-foreground">
          {view.live} of {view.limit} previews live, each a copy of <span className="text-foreground">{view.settings.start_from}</span>
          {full ? <>. <Link to={PREVIEW_CLOSE_ROUTE_TO} params={params} className="text-foreground underline underline-offset-4">Close one</Link> to deploy another.</> : null}
        </p>
        <ToggleGroup variant="outline" size="sm" spacing={0} aria-label="Pull requests" value={[filter]}
          onValueChange={(next) => { const [picked] = next; if (picked === "open" || picked === "closed") onFilter(picked); }}>
          <ToggleGroupItem value="open">Open</ToggleGroupItem>
          <ToggleGroupItem value="closed">Closed</ToggleGroupItem>
        </ToggleGroup>
      </div>
      {rows.length === 0 ? (
        <Empty variant="placeholder">
          <EmptyHeader>
            <EmptyTitle>{filter === "open" ? "No open pull requests" : "No closed pull requests"}</EmptyTitle>
            {filter === "open" ? <EmptyDescription>Open one on {view.repository} and it shows here.</EmptyDescription> : null}
          </EmptyHeader>
        </Empty>
      ) : (
        <Table aria-label="Pull requests">
          <TableHeader>
            <TableRow>
              <TableHead>Pull request</TableHead>
              <TableHead>Preview</TableHead>
              <TableHead className="max-sm:hidden">Updated</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((row) => (
              <TableRow key={row.number}>
                <TableCell className="w-1/2 max-w-0">
                  <div className="flex min-w-0 flex-col">
                    <Link to={PREVIEW_ROUTE_TO} params={{ ...params, number: String(row.number) }} className="truncate hover:underline">
                      #{row.number} {row.title}
                    </Link>
                    <span className="truncate text-muted-foreground">{authorLine(row)}</span>
                  </div>
                </TableCell>
                <TableCell className="whitespace-normal">
                  <PreviewStatus line={outcomeLine(row, view, organizationName)}>
                    <RowAction row={row} view={view} params={params} />
                  </PreviewStatus>
                </TableCell>
                <TableCell className="text-muted-foreground max-sm:hidden"><RelativeTime date={new Date(row.updated)} /></TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </div>
  );
}

/** The one action that changes a row's outcome: Open a live preview, Deploy one that can, or make room for it. */
function RowAction({ row, view, params }: { row: PreviewRow; view: PreviewsView; params: PreviewRouteParams }) {
  const writes = usePreviewWrites(params.organizationSlug);
  const [deploying, setDeploying] = useState(false);
  const outcome = row.preview;
  if (outcome.state === "live" && outcome.newer === undefined) {
    return outcome.address
      ? <Button variant="link" size="sm" render={<a href={addressHref(outcome.address)} target="_blank" rel="noreferrer" />}>Open</Button>
      : null;
  }
  if (outcome.state === "not_deployed" && outcome.why?.kind === "full" && row.open) {
    return (
      <>
        <Button variant="link" size="sm" render={<Link to={PREVIEW_CLOSE_ROUTE_TO} params={params} search={{ for: row.number }} />}>Close one</Button>
        <Button variant="link" size="sm" render={<Link to={PREVIEW_SETTINGS_ROUTE_TO} params={params} search={{ edit: true }} />}>Raise the limit</Button>
      </>
    );
  }
  if (outcome.state === "not_deployed" && outcome.why?.kind === "no_server") {
    return <Button variant="link" size="sm" render={<Link to={PREVIEW_SETTINGS_ROUTE_TO} params={params} search={{ edit: true }} />}>Change servers</Button>;
  }
  const head = row.head;
  if (!deployable(row, view) || head === null) return null;
  const deploy = async () => {
    setDeploying(true);
    try {
      await writes.deploy({ repository_id: view.repository_id, number: row.number, head });
      toast.success(`Deploying #${row.number}`);
    } catch (error) {
      toast.error(writeFailure(error));
    } finally {
      setDeploying(false);
    }
  };
  return (
    <Button variant="link" size="sm" disabled={deploying} onClick={() => void deploy()}>
      {outcome.state === "live" ? "Deploy again" : "Deploy"}
    </Button>
  );
}
