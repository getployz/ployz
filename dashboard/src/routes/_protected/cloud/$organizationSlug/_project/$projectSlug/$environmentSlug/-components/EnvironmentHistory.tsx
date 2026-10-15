import { useRef, useState, useTransition } from "react";
import type { ChangeKind, HistoryAction, HistoryPreview, SavedRevision } from "@ployz/sdk";
import { linkOptions, useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import { HistoryIcon, MoreVerticalIcon, Undo2Icon } from "lucide-react";
import { toast } from "sonner";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { RelativeTime } from "#/components/relative-time";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Spinner } from "#/components/ui/spinner";
import { listNames, plural } from "#/lib/plural";
import { cn } from "#/lib/utils";
import { type ChangeGroup, changeGroups } from "#/modules/config-store/store-deployments";
import { useStoreBusy } from "#/modules/config-store/store-pending";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { diffQuery, requireView, storeViewOptions, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { DashboardPage } from "#/components/dashboard-page";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";
import { kindText, Value } from "./canvas/EnvironmentChangesReview";
import { notSetToast, sealedOnlyPaths } from "./history-not-set";

/** How many more revisions each Show older reads. */
const HISTORY_PAGE = 50;
/** How many changes a card shows before it folds the rest. */
const SHOWN_CHANGES = 3;

type HistoryReview =
  | { kind: "closed" }
  | { kind: "loading" }
  | { kind: "preview"; preview: HistoryPreview; error?: string }
  | { kind: "failed"; message: string };

/** One change a save made, by the Store path its row undo names. */
type Line = {
  path: string;
  kind: ChangeKind;
  node: string;
  name: string;
  variable: boolean;
  lifecycle: boolean;
  before: string;
  after: string;
};

function lines(groups: ChangeGroup[]): Line[] {
  return groups.flatMap((group): Line[] => {
    // An added or removed node is one change; its own settings came and go with it.
    if (group.lifecycle !== "update") {
      return [{
        path: group.discardPath, kind: group.lifecycle === "create" ? "add" : "remove", node: group.nodeName,
        name: group.nodeName, variable: false, lifecycle: true, before: "", after: "",
      }];
    }
    return group.rows.map((row) => ({
      path: row.path, kind: row.kind, node: group.nodeName, name: row.name, variable: row.variable, lifecycle: false,
      before: row.currentValue, after: row.newValue,
    }));
  });
}

const unique = (names: string[]) => [...new Set(names)];

/** What a save did, from its changes, when nobody titled it. */
function saveTitle(changes: Line[], revision: number) {
  if (!changes.length) return `Saved version #${revision}`;
  const nodes = unique(changes.map((line) => line.node));
  const lifecycle = changes.filter((line) => line.lifecycle);
  if (lifecycle.length) {
    const verb = lifecycle.every((line) => line.kind === "add") ? "Add" : lifecycle.every((line) => line.kind === "remove") ? "Remove" : "Change";
    return `${verb} ${listNames(lifecycle.map((line) => line.name))}`;
  }
  const names = unique(changes.map((line) => line.name));
  if (changes.length === 1) return `Change ${changes[0]?.node} ${changes[0]?.name}`;
  if (nodes.length === 1) return `Update ${nodes[0]} ${listNames(names)}`;
  if (names.length === 1) return `Set ${names[0]} on ${listNames(nodes)}`;
  return `Update ${listNames(nodes)}`;
}

/** "#9 and #10": the saves after this one, oldest first. */
function later(history: SavedRevision[], revision: number) {
  return listNames(history.filter((saved) => saved.revision > revision).map((saved) => `#${saved.revision}`).reverse());
}

/**
 * The Environment's Saved versions, newest first. `revision` picks one out: it reads far enough back to hold it, then
 * scrolls to it. Undo and Roll back stage onto the canvas, so a change goes to Architecture to review it.
 */
export function EnvironmentHistory({ revision: picked }: { revision?: number }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
  const diff = requireView(useStoreView(params.organizationSlug, diffQuery(store)));
  // Revisions count up, so the saves since the picked one bound how many to read.
  const [limit, setLimit] = useState<number | null>(() =>
    picked !== undefined && diff.saved !== null && diff.saved >= picked ? Math.max(diff.saved - picked + 1, HISTORY_PAGE) : null);
  const [loadingOlder, startOlder] = useTransition();
  const history = requireView(useStoreView(params.organizationSlug,
    limit === null ? { query: "history", environment: store } : { query: "history", environment: store, limit }));
  const scope = useCollectionScope();
  const writer = useStoreWriter(params.organizationSlug);
  const request = useRef(0);
  const [review, setReview] = useState<HistoryReview>({ kind: "closed" });
  const [unfolded, setUnfolded] = useState<ReadonlySet<number>>(new Set());
  const restoring = useStoreBusy(params.organizationSlug, store).has("restoring");

  async function stage(preview: HistoryPreview, acceptOverwrite: boolean) {
    setReview({ kind: "preview", preview });
    try {
      const written = await writer.commit({ command: "stage_history", environment: store, revision: preview.revision,
        action: preview.action, change: preview.change, version: preview.version, accept_overwrite: acceptOverwrite },
      ["conflict", "confirmation_required"]).isPersisted.promise;
      if (written.written === "history_staged") {
        const notSet = notSetToast(preview.action, written.not_set);
        if (notSet) toast(notSet.title, { description: notSet.description });
        if (written.changed) void navigate(linkOptions({ to: ENVIRONMENT_INDEX_ROUTE_TO, params, search: {} }));
      }
      setReview({ kind: "closed" });
    } catch (error) {
      setReview({ kind: "preview", preview,
        error: error instanceof StoreRefused ? error.message : "Could not stage this version. Try again." });
    }
  }

  async function choose(revision: number, action: HistoryAction, change?: string) {
    const requested = ++request.current;
    setReview({ kind: "loading" });
    try {
      const result = await scope.queryClient.fetchQuery({
        ...storeViewOptions(params.organizationSlug, scope, { query: "history_preview", environment: store, revision, action, change }),
        staleTime: 0,
      });
      if (requested !== request.current) return;
      const preview = requireView(result);
      if (preview.overwritten.length) setReview({ kind: "preview", preview });
      else await stage(preview, false);
    } catch (error) {
      if (requested !== request.current) return;
      setReview({ kind: "failed", message: error instanceof Error ? error.message : "Could not review this version." });
    }
  }

  const close = () => { request.current += 1; setReview({ kind: "closed" }); };
  const undoing = review.kind === "preview" && review.preview.action === "undo";
  return (
    <DashboardPage width="content">
      <div className="flex flex-col gap-3">
        {restoring && !(review.kind === "preview" && review.preview.overwritten.length) ? (
          <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground"><Spinner />Restoring…</p>
        ) : review.kind === "loading" ? <p role="status" className="text-sm text-muted-foreground">Reviewing version...</p> : null}
        {review.kind === "failed" ? <p role="alert">{review.message}</p> : null}
        {review.kind === "preview" && review.error && !review.preview.overwritten.length ? <p role="alert">{review.error}</p> : null}
        {!history.revisions.length ? <Empty variant="placeholder"><EmptyDescription>No saved versions yet</EmptyDescription></Empty> : null}
        <ol className="flex flex-col gap-3" aria-label="Saved versions">
          {history.revisions.map((revision) => {
            const changes = lines(changeGroups({ ...diff, changes: revision.changes }, []));
            const current = revision.revision === diff.saved;
            const after = later(history.revisions, revision.revision);
            const open = unfolded.has(revision.revision);
            const shown = open ? changes : changes.slice(0, SHOWN_CHANGES);
            const undoable = revision.predecessor !== null;
            const sealed = sealedOnlyPaths(revision.changes, revision.linked);
            const nothingToUndo = changes.length > 0 && changes.every((line) => sealed.has(line.path));
            return (
              <li key={revision.revision} aria-current={revision.revision === picked || undefined}
                ref={revision.revision === picked ? (node) => node?.scrollIntoView({ block: "center" }) : undefined}
                className={cn("scroll-mt-4 rounded-lg border p-3", revision.revision === picked && "ring-2 ring-ring")}>
                <div className="flex items-start justify-between gap-2">
                  <div className="min-w-0">
                    <p className="flex flex-wrap items-center gap-2 font-medium">
                      {revision.message ?? saveTitle(changes, revision.revision)}
                      {current ? <Badge variant="secondary">Current</Badge> : null}
                    </p>
                    <p className="text-xs text-muted-foreground">
                      #{revision.revision}{revision.saved_by ? ` · ${revision.saved_by}` : ""}
                      {revision.saved_at !== null ? <> · <RelativeTime date={new Date(revision.saved_at * 1000)} /></> : null}
                    </p>
                  </div>
                  {changes.length || !current ? (
                    <DropdownMenu>
                      <DropdownMenuTrigger disabled={restoring} render={<Button size="icon-sm" variant="ghost" aria-label={`Actions for version ${revision.revision}`} />}>
                        <MoreVerticalIcon />
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end" className="w-auto">
                        {changes.length ? (
                          <DropdownMenuItem disabled={!undoable || nothingToUndo} onClick={() => void choose(revision.revision, "undo")}>
                            <Undo2Icon />
                            <span>
                              {changes.length === 1 ? "Undo this change" : `Undo these ${plural(changes.length, "change")}`}
                              {nothingToUndo ? <span className="block text-xs text-muted-foreground">Sealed. Nothing to undo.</span>
                                : after ? <span className="block text-xs text-muted-foreground">Keeps {after}</span> : null}
                            </span>
                          </DropdownMenuItem>
                        ) : null}
                        {current ? null : (
                          <DropdownMenuItem onClick={() => void choose(revision.revision, "restore")}>
                            <HistoryIcon />
                            <span>
                              Roll back to #{revision.revision}
                              {after ? <span className="block text-xs text-muted-foreground">Undoes {after}</span> : null}
                            </span>
                          </DropdownMenuItem>
                        )}
                      </DropdownMenuContent>
                    </DropdownMenu>
                  ) : null}
                </div>
                {changes.length ? (
                  <ul className="mt-2" aria-label={`Changes in version ${revision.revision}`}>
                    {shown.map((line) => {
                      const linked = revision.linked[line.path] ?? 1;
                      return (
                        <li key={line.path} className="grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-x-2 py-0.5 text-sm">
                          {line.lifecycle ? (
                            <span className={cn("truncate", kindText[line.kind])}>{line.kind === "add" ? "+" : "−"} {line.name}</span>
                          ) : (
                            <span className="flex min-w-0 items-baseline gap-1.5">
                              <span className="shrink-0 text-muted-foreground">{line.node}</span>
                              <span className={cn("truncate", line.variable && "font-mono")}>{line.name}</span>
                            </span>
                          )}
                          {line.lifecycle ? <span /> : <Value kind={line.kind} before={line.before} after={line.after} />}
                          {undoable ? (
                            <DropdownMenu>
                              <DropdownMenuTrigger render={<Button size="icon-xs" variant="ghost" className="text-muted-foreground" aria-label={`Actions for ${line.node} ${line.name} in version ${revision.revision}`} />}>
                                <MoreVerticalIcon />
                              </DropdownMenuTrigger>
                              <DropdownMenuContent align="end" className="w-auto">
                                <DropdownMenuItem disabled={sealed.has(line.path)} onClick={() => void choose(revision.revision, "undo", line.path)}>
                                  <Undo2Icon />
                                  <span>
                                    {linked > 1 ? `Undo ${plural(linked, "change")}` : "Undo change"}
                                    {sealed.has(line.path) ? <span className="block text-xs text-muted-foreground">Sealed. Nothing to undo.</span> : null}
                                  </span>
                                </DropdownMenuItem>
                              </DropdownMenuContent>
                            </DropdownMenu>
                          ) : <span />}
                        </li>
                      );
                    })}
                  </ul>
                ) : null}
                {changes.length > SHOWN_CHANGES ? (
                  <Button variant="ghost" size="xs" className="mt-1 text-muted-foreground"
                    onClick={() => setUnfolded((set) => { const next = new Set(set); if (open) next.delete(revision.revision); else next.add(revision.revision); return next; })}>
                    {open ? "Show less" : `${changes.length - SHOWN_CHANGES} more`}
                  </Button>
                ) : null}
              </li>
            );
          })}
        </ol>
        {history.more ? (
          <Button variant="outline" size="sm" className="self-center" disabled={loadingOlder}
            onClick={() => startOlder(() => setLimit(history.revisions.length + HISTORY_PAGE))}>
            {loadingOlder ? "Loading..." : "Show older"}
          </Button>
        ) : null}
      </div>
      {review.kind === "preview" && review.preview.overwritten.length > 0 ? (
        <Dialog open onOpenChange={(open) => { if (!open && !restoring) close(); }}>
          <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto">
            <DialogHeader>
              <DialogTitle>Overwrite draft changes?</DialogTitle>
              <DialogDescription>{undoing ? "Undoing" : "Rolling back"} will overwrite these draft changes:</DialogDescription>
            </DialogHeader>
            <ul className="max-h-72 overflow-y-auto break-words list-inside list-disc text-sm">{review.preview.overwritten.map((path) => <li key={path}>{path}</li>)}</ul>
            {review.preview.version !== diff.version ? <p role="alert">The canvas changed. Cancel and review this version again.</p> : null}
            {review.error ? <p role="alert">{review.error}</p> : null}
            <DialogFooter>
              <Button variant="outline" disabled={restoring} onClick={close}>Cancel</Button>
              <Button disabled={restoring || review.preview.version !== diff.version}
                onClick={() => void stage(review.preview, true)}>
                {restoring ? <><Spinner data-icon="inline-start" />Restoring…</> : undoing ? "Undo" : "Roll back"}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      ) : null}
    </DashboardPage>
  );
}
