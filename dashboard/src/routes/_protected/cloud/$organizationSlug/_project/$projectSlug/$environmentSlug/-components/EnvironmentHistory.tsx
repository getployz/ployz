import { useRef, useState } from "react";
import type { HistoryAction, HistoryPreview } from "@ployz/sdk";
import { useLoaderData, useParams } from "@tanstack/react-router";
import { MoreVerticalIcon, RotateCcwIcon, Undo2Icon } from "lucide-react";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { RelativeTime } from "#/components/relative-time";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { changeGroups } from "#/modules/config-store/store-deployments";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { diffQuery, requireView, storeViewOptions, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { CanvasInspectorHeader } from "./CanvasInspectorHeader";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";
import { ChangeGroups } from "./canvas/EnvironmentChangesReview";

type HistoryReview =
  | { kind: "closed" }
  | { kind: "loading" }
  | { kind: "preview"; preview: HistoryPreview; saving: boolean; error?: string }
  | { kind: "failed"; message: string };

export function EnvironmentHistory() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const history = requireView(useStoreView(params.organizationSlug, { query: "history", environment: store }));
  const diff = requireView(useStoreView(params.organizationSlug, diffQuery(store)));
  const scope = useCollectionScope();
  const writer = useStoreWriter(params.organizationSlug);
  const request = useRef(0);
  const [review, setReview] = useState<HistoryReview>({ kind: "closed" });

  async function stage(preview: HistoryPreview, acceptOverwrite: boolean) {
    setReview({ kind: "preview", preview, saving: true });
    try {
      await writer.commit({ command: "stage_history", environment: store, revision: preview.revision,
        action: preview.action, version: preview.version, accept_overwrite: acceptOverwrite },
      ["conflict", "confirmation_required"]).isPersisted.promise;
      setReview({ kind: "closed" });
    } catch (error) {
      setReview({ kind: "preview", preview, saving: false,
        error: error instanceof StoreRefused ? error.message : "Could not stage this version. Try again." });
    }
  }

  async function choose(revision: number, action: HistoryAction) {
    const requested = ++request.current;
    setReview({ kind: "loading" });
    try {
      const result = await scope.queryClient.fetchQuery({
        ...storeViewOptions(params.organizationSlug, scope, { query: "history_preview", environment: store, revision, action }),
        staleTime: 0,
      });
      if (requested !== request.current) return;
      const preview = requireView(result);
      if (preview.overwritten.length) setReview({ kind: "preview", preview, saving: false });
      else await stage(preview, false);
    } catch (error) {
      if (requested !== request.current) return;
      setReview({ kind: "failed", message: error instanceof Error ? error.message : "Could not review this version." });
    }
  }

  const close = () => { request.current += 1; setReview({ kind: "closed" }); };
  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}><span className="font-medium">History</span></CanvasInspectorHeader>
      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4">
        {review.kind === "loading" || (review.kind === "preview" && review.saving && !review.preview.overwritten.length) ? <p role="status" className="text-sm text-muted-foreground">Reviewing version...</p> : null}
        {review.kind === "failed" ? <p role="alert">{review.message}</p> : null}
        {review.kind === "preview" && review.error && !review.preview.overwritten.length ? <p role="alert">{review.error}</p> : null}
        {!history.revisions.length ? <Empty variant="placeholder"><EmptyDescription>No saved versions yet</EmptyDescription></Empty> : null}
        <ol className="flex flex-col gap-3" aria-label="Saved versions">
          {history.revisions.map((revision) => (
            <li key={revision.revision} className="rounded-lg border p-3">
              <div className="flex items-start justify-between gap-2">
                <div className="min-w-0">
                  <p className="flex flex-wrap items-center gap-2 font-medium">
                    {revision.message ?? `Saved version #${revision.revision}`}
                    {revision.revision === diff.saved ? <Badge variant="secondary">Current</Badge> : null}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    #{revision.revision}{revision.saved_by ? ` · ${revision.saved_by}` : ""}
                    {revision.saved_at !== null ? <> · <RelativeTime date={new Date(revision.saved_at * 1000)} /></> : null}
                  </p>
                </div>
                <DropdownMenu>
                  <DropdownMenuTrigger render={<Button size="icon-sm" variant="ghost" aria-label={`Actions for version ${revision.revision}`} />}>
                    <MoreVerticalIcon />
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end" className="w-auto">
                    <DropdownMenuItem disabled={revision.predecessor === null} onClick={() => void choose(revision.revision, "undo")}>
                      <Undo2Icon />Undo this change
                    </DropdownMenuItem>
                    <DropdownMenuItem onClick={() => void choose(revision.revision, "restore")}><RotateCcwIcon />Restore this version</DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>
              {revision.predecessor === null ? <p className="mt-2 text-xs text-muted-foreground">Initial saved version. No earlier version is recorded to undo.</p> : (
                <details className="mt-2">
                  <summary className="cursor-pointer text-sm text-muted-foreground">{revision.total_count} changes</summary>
                  <ChangeGroups groups={changeGroups({ ...diff, changes: revision.changes }, [])} />
                </details>
              )}
            </li>
          ))}
        </ol>
      </div>
      {review.kind === "preview" && review.preview.overwritten.length > 0 ? (
        <Dialog open onOpenChange={(open) => { if (!open && !review.saving) close(); }}>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Overwrite draft changes?</DialogTitle>
              <DialogDescription>{review.preview.action === "restore" ? "Restoring this version" : "Undoing this change"} will overwrite these draft changes:</DialogDescription>
            </DialogHeader>
            <ul className="list-inside list-disc text-sm">{review.preview.overwritten.map((path) => <li key={path}>{path}</li>)}</ul>
            {review.preview.version !== diff.version ? <p role="alert">The canvas changed. Cancel and review this version again.</p> : null}
            {review.error ? <p role="alert">{review.error}</p> : null}
            <DialogFooter>
              <Button variant="outline" disabled={review.saving} onClick={close}>Cancel</Button>
              <Button disabled={review.saving || review.preview.version !== diff.version}
                onClick={() => void stage(review.preview, true)}>{review.saving ? "Staging..." : review.preview.action === "restore" ? "Restore" : "Undo"}</Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      ) : null}
    </div>
  );
}
