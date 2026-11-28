import { useState, type ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { toast } from "sonner";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { RelativeTime } from "#/components/relative-time";
import { RouteErrorAlert } from "#/components/route-error-alert";
import { Button } from "#/components/ui/button";
import { Skeleton } from "#/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "#/components/ui/table";
import { cn } from "#/lib/utils";
import { PREVIEW_ROUTE_TO, PREVIEW_SETTINGS_ROUTE_TO, type PreviewRouteParams } from "#/lib/environment-routes";
import type { PreviewOutcome, PreviewRow, PreviewsView } from "#/modules/previews/previews.contract";
import { usePreviewWrites, writeFailure } from "#/modules/previews/preview-writes";
import { useServerList } from "#/modules/machines/use-servers";

/** A Server's name by its Machine ID, or the ID's start while the list doesn't have it. */
export function useServerName(organizationSlug: string) {
  const { servers } = useServerList(organizationSlug);
  return (id: string) => servers.find((server) => server.machine.id === id)?.name ?? id.slice(0, 8);
}

export const shortCommit = (sha: string) => sha.slice(0, 7);

/** Where a live preview answers, as a link opens it. */
export const addressHref = (address: string) => (address.includes("://") ? address : `https://${address}`);

/** A pull request's author as a row names them: a fork's by its GitHub login. */
export const authorLine = (row: PreviewRow) => (row.fork ? `@${row.author}, from a fork` : row.author);

/** Whether a row's preview is up or on its way: it counts toward the limit and can be closed. */
export const isUp = (outcome: PreviewOutcome): outcome is Extract<PreviewOutcome, { state: "live" | "deploying" }> =>
  outcome.state === "live" || outcome.state === "deploying";

/** The live previews, by last push, newest first: what Close a preview lists. */
export const upRows = (view: PreviewsView) =>
  view.pull_requests.filter((row) => isUp(row.preview)).sort((a, b) => b.updated.localeCompare(a.updated));

export type Tone = "success" | "info" | "warning" | "muted";

/** A pull request's one outcome line: Live, Deploying, Not deployed and why, or Closed; `detail` qualifies it. */
export type OutcomeLine = { readonly tone: Tone; readonly text: string; readonly detail?: string };

export function outcomeLine(row: PreviewRow, view: PreviewsView, organizationName: string): OutcomeLine {
  const startFrom = view.settings?.start_from ?? "";
  const outcome = row.preview;
  switch (outcome.state) {
    case "live":
      return {
        tone: "success",
        text: outcome.address ? `Live at ${outcome.address}` : "Live",
        detail: outcome.newer ? `new commits since ${shortCommit(outcome.head)}` : undefined,
      };
    case "deploying":
      return { tone: "info", text: "Deploying", detail: shortCommit(outcome.head) };
    case "closed":
      return { tone: "muted", text: "Closed" };
    case "not_deployed": {
      const why = outcome.why;
      if (!view.settings || why?.kind === "off") return { tone: "muted", text: "Not deployed" };
      if (!row.open) return { tone: "muted", text: "Not deployed", detail: "pull request closed" };
      switch (why?.kind) {
        case undefined: return { tone: "muted", text: "Not deployed" };
        case "full": return { tone: "warning", text: `Not deployed: ${why.live} of ${why.limit} previews are live` };
        case "not_covered": return { tone: "warning", text: `Not deployed: it changes ${why.service}'s settings`, detail: "Preview settings don't cover that" };
        case "merges_into_start_from": return { tone: "muted", text: `Not deployed: it merges into ${startFrom}'s own branch` };
        case "no_server": return { tone: "warning", text: "Not deployed: no server can hold another preview" };
        case "not_member": return { tone: "muted", text: `Not deployed: ${why.author} isn't in ${organizationName} on Ployz` };
        case "failed": return { tone: "warning", text: `Not deployed: ${why.message}` };
      }
    }
  }
}

const DOT = { success: "bg-success", info: "bg-info", warning: "bg-warning", muted: "bg-muted-foreground" } satisfies Record<Tone, string>;

/** An outcome line with its coloured dot. */
export function PreviewStatus({ line, children }: { line: OutcomeLine; children?: ReactNode }) {
  return (
    <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
      <span aria-hidden="true" className={cn("size-2 shrink-0 rounded-full", DOT[line.tone])} />
      <span>{line.text}</span>
      {line.detail ? <span className="text-muted-foreground">{line.detail}</span> : null}
      {children}
    </span>
  );
}

/** Whether a Deploy can go now: the pull request is open, has a commit, and nothing the Store said holds it. */
export function deployable(row: PreviewRow, view: PreviewsView) {
  if (!view.settings || !row.open || row.head === null) return false;
  if (row.preview.state === "live") return row.preview.newer !== undefined;
  if (row.preview.state !== "not_deployed") return false;
  const why = row.preview.why?.kind;
  return why === undefined || why === "failed" || (why === "full" && view.live < view.limit);
}

/** The page's view couldn't be read: the Store's refusal, worded as it is. */
export function PreviewsRefused({ message }: { message: string }) {
  return <RouteErrorAlert title="Previews couldn't load" description={message} />;
}

export function PreviewsPending() {
  return (
    <div role="status" aria-label="Loading" className="mx-auto flex w-full max-w-5xl flex-col gap-3 px-4 py-6 md:px-6 md:py-8">
      <Skeleton className="h-8 w-48 max-w-full" />
      {Array.from({ length: 4 }, (_, index) => <Skeleton key={index} className="h-12 w-full" />)}
    </div>
  );
}

export function PreviewsError() {
  return (
    <div className="mx-auto w-full max-w-5xl px-4 py-6 md:px-6 md:py-8">
      <RouteErrorAlert title="Previews couldn't load" description="Try loading the page again." />
    </div>
  );
}

/**
 * Closing one preview, after a confirm that says what goes: its copy and databases. The pull request stays open and
 * can be deployed again.
 */
export function useClosePreview(organizationSlug: string, project: string) {
  const writes = usePreviewWrites(organizationSlug);
  const [closing, setClosing] = useState<PreviewRow | null>(null);
  const environment = closing && isUp(closing.preview) ? closing.preview.environment : null;
  const dialog = (
    <ConfirmDialog
      open={closing !== null}
      onOpenChange={(open) => { if (!open) setClosing(null); }}
      title={closing ? `Close the preview of #${closing.number}?` : ""}
      description="Its copy and databases are deleted. The pull request stays open, so you can deploy it again."
      actionLabel="Close preview"
      pendingLabel="Closing…"
      variant="destructive"
      onConfirm={async () => {
        if (closing === null || environment === null) return;
        try {
          await writes.close({ project, environment });
          toast.success(`Closed the preview of #${closing.number}`);
          setClosing(null);
        } catch (error) {
          toast.error(writeFailure(error));
        }
      }}
    />
  );
  return { ask: setClosing, dialog };
}

/** The live previews, by last push, each with Close: how to make room for another. */
export function CloseOneList({ view, params, project, organizationSlug }: {
  view: PreviewsView;
  params: PreviewRouteParams;
  project: string;
  organizationSlug: string;
}) {
  const close = useClosePreview(organizationSlug, project);
  const rows = upRows(view);
  return (
    <div className="flex flex-col gap-3">
      <Table aria-label="Live previews">
        <TableHeader>
          <TableRow>
            <TableHead>Live preview</TableHead>
            <TableHead className="max-sm:hidden">Last push</TableHead>
            <TableHead><span className="sr-only">Close</span></TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.map((row) => (
            <TableRow key={row.number}>
              <TableCell className="max-w-0 w-full">
                <div className="flex min-w-0 flex-col">
                  <Link to={PREVIEW_ROUTE_TO} params={{ ...params, number: String(row.number) }} className="truncate hover:underline">
                    #{row.number} {row.title}
                  </Link>
                  <span className="truncate text-muted-foreground">
                    {row.preview.state === "live"
                      ? (row.preview.address ? <span className="font-mono">{row.preview.address}</span> : "Live")
                      : "Deploying"}
                  </span>
                </div>
              </TableCell>
              <TableCell className="text-muted-foreground max-sm:hidden">
                <RelativeTime date={new Date(row.updated)} />, {row.fork ? `@${row.author}` : row.author}
              </TableCell>
              <TableCell className="text-right">
                <Button variant="outline" size="sm" onClick={() => close.ask(row)}>Close</Button>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      <p className="text-sm text-muted-foreground">Closing deletes that preview's copy and databases. Its pull request stays open.</p>
      {close.dialog}
    </div>
  );
}

/** Raise the limit: Preview settings, open for editing. */
export function RaiseLimit({ params }: { params: PreviewRouteParams }) {
  return (
    <Button variant="outline" render={<Link to={PREVIEW_SETTINGS_ROUTE_TO} params={params} search={{ edit: true }} />}>
      Raise the limit
    </Button>
  );
}
