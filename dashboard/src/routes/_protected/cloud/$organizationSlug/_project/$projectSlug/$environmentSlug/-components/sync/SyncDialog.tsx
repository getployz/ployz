import { useState } from "react";
import type { EnvironmentRef, NeverSyncedRow, SyncRow } from "@ployz/sdk";
import { ArrowRightIcon, ChevronDownIcon, HardDriveIcon, PackageIcon, PinIcon, PinOffIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import { plural } from "#/lib/plural";
import { cn } from "#/lib/utils";
import { nodeName } from "#/modules/config-store/store-branches";
import { isWholeNode, syncLine, syncName, syncPicks, syncRowPath, syncSections } from "#/modules/config-store/store-sync";
import { syncQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { StoreRefused } from "#/modules/config-store/store.contract";

/**
 * The one review of a Sync: each change `from` would put in `into` as one row, ticked as the Store suggests. Unticked,
 * a change stays out this time and can be marked Never sync; the footer lists what is never synced, with undo.
 * Nothing deploys: the changes become `into`'s changes to deploy, or, from a PR Environment into its Destination, go
 * live there when the pull request merges. A secret `into` lacks arrives by name only: its row takes `into`'s own
 * value, set now or held for the merge. Monochrome: pink stays for staged intent and Deploy.
 */
export function SyncDialog({ organizationSlug, from, into, parent, closable, onClose, onSynced }: {
  organizationSlug: string; from: EnvironmentRef; into: string; parent: string;
  /** Offer to close `from` once synced: a Branch that isn't kept, into its Parent. */
  closable: boolean;
  onClose: () => void;
  /** `atMerge`: the pull request whose merge they go live with; null when they were staged now. */
  onSynced: (rows: readonly SyncRow[], atMerge: number | null) => void;
}) {
  const writer = useStoreWriter(organizationSlug);
  // Into its Parent, the view the page prefetched.
  const view = useStoreView(organizationSlug, syncQuery(from, into === parent ? null : into));
  // Rows the user flipped from their default, by key: they survive a refetch.
  const [flipped, setFlipped] = useState<ReadonlySet<string>>(new Set());
  // `into`'s values typed for secret rows, by key: sent after the Sync, never shown back.
  const [values, setValues] = useState<Readonly<Record<string, string>>>({});
  const [closeAfter, setCloseAfter] = useState(true);
  const [pending, setPending] = useState(false);
  const [stale, setStale] = useState(false);
  // The never-synced list, opened from the footer.
  const [listing, setListing] = useState(false);
  const name = from.environment ?? "";
  const rows = view.ok ? view.value.rows : [];
  const atMerge = view.ok ? view.value.at_merge : null;
  // A PR Environment closes with its pull request.
  const closing = closable && atMerge === null;
  const picked = syncPicks(rows, flipped);
  const wholes = new Map(rows.filter(isWholeNode).map((row) => [row.node, picked.includes(row)]));
  const flip = (key: string) => setFlipped((current) => {
    const next = new Set(current);
    if (!next.delete(key)) next.add(key);
    return next;
  });
  const neverSync = (environment: string, path: string, off: boolean) =>
    writer.commit({ command: "never_sync", environment: { project: from.project, environment }, paths: [path], off });

  async function sync() {
    if (!view.ok) return;
    setPending(true);
    setStale(false);
    try {
      // Awaited: the page opens the receiver once it holds the changes, and a stale review stays open, refetched.
      await writer.commit({
        command: "sync", from, into: { project: from.project, environment: into }, picks: picked.map((row) => row.key),
        version: view.value.version, close_after: closing && closeAfter,
      }, ["conflict"]).isPersisted.promise;
    } catch (error) {
      // Any other refusal is the writer's toast.
      setStale(error instanceof StoreRefused && error.code === "conflict");
      setPending(false);
      return;
    }
    // Once synced: a value now sets the secret, one at the merge is held for it.
    const receiver = { project: from.project, environment: into };
    const typed = picked.flatMap((row) => {
      const value = row.secret ? values[row.key] : undefined;
      return value ? [{ path: syncRowPath(row), value }] : [];
    });
    if (atMerge !== null) {
      for (const { path, value } of typed) writer.commit({ command: "hold_secret", environment: receiver, pull_request: atMerge, path, value });
    } else if (typed.length) {
      writer.commit({
        command: "edit", environment: receiver, expect: null,
        changes: typed.map(({ path, value }) => ({ op: "set", path, value: { secret: value } })),
      });
    }
    onSynced(picked, atMerge);
  }

  const neverSynced = view.ok ? view.value.never_synced : [];
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent padding="none" className="flex max-h-[85dvh] flex-col overflow-hidden sm:max-w-xl">
        <DialogHeader className="shrink-0 px-6 pt-5">
          <DialogTitle className="pr-8">Sync to {into}</DialogTitle>
          <DialogDescription>
            {!view.ok ? view.refusal.message : rows.length === 0 ? `Nothing to sync: ${into} has every change from ${name}.`
              : atMerge !== null ? `These changes from ${name} go live in ${into} when #${atMerge} merges.`
              : `These changes from ${name} become ${into}'s changes to deploy.`}
          </DialogDescription>
        </DialogHeader>
        <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-4">
          {syncSections(rows).map((section) => (
            <section key={section.node} aria-label={nodeName(section.node)} className="mt-4">
              <h3 className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
                {section.node.startsWith("volumes.") ? <HardDriveIcon className="size-3.5" /> : <PackageIcon className="size-3.5" />}
                {nodeName(section.node)}
              </h3>
              <ul>
                {section.rows.map((row) => {
                  const follows = isWholeNode(row) ? undefined : wholes.get(row.node);
                  return (
                    <SyncRowItem key={row.key} row={row} into={into} ticked={follows ?? picked.includes(row)} follows={follows !== undefined}
                      onFlip={() => flip(row.key)} onNeverSync={() => neverSync(name, syncRowPath(row), false)}
                      value={values[row.key] ?? ""} onValue={(value) => setValues((current) => ({ ...current, [row.key]: value }))} />
                  );
                })}
              </ul>
            </section>
          ))}
          {stale ? <p role="status" className="mt-4 text-muted-foreground">These changed since you opened them. Here they are now.</p> : null}
        </div>
        {listing && neverSynced.length ? (
          <ul id="never-synced" aria-label="Never synced" className="max-h-40 shrink-0 overflow-y-auto border-t px-6 py-1">
            {neverSynced.map((row) => <NeverSyncedItem key={row.key} row={row} onSyncAgain={() => {
              for (const environment of row.marked_in) neverSync(environment, syncRowPath(row), true);
            }} />)}
          </ul>
        ) : null}
        {closing && rows.length ? (
          <label className="flex shrink-0 items-center gap-3 border-t px-6 py-3">
            <Checkbox checked={closeAfter} onCheckedChange={setCloseAfter} />
            Close {name} after syncing
          </label>
        ) : null}
        <DialogFooter className="m-0 shrink-0 px-6 py-3 sm:items-center">
          {neverSynced.length ? (
            <Button variant="ghost" className="text-muted-foreground sm:mr-auto" aria-expanded={listing} aria-controls="never-synced"
              onClick={() => setListing(!listing)}>
              {neverSynced.length} never synced<ChevronDownIcon data-icon="inline-end" className={cn(listing && "rotate-180")} />
            </Button>
          ) : null}
          {rows.length ? <>
            <Button variant="outline" onClick={onClose}>Cancel</Button>
            <Button disabled={picked.length === 0 || pending} onClick={() => void sync()}>
              {pending ? <Spinner data-icon="inline-start" /> : null}Sync {plural(picked.length, "change")}
            </Button>
          </> : <Button variant="outline" onClick={onClose}>Done</Button>}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/**
 * One change: tick, name (a variable's key in monospace), at most one badge, and old → new on the right; a secret takes
 * `into`'s value there instead. Unticked, it dims and offers Never sync instead. A setting of a new node follows the
 * node's tick.
 */
function SyncRowItem({ row, into, ticked, follows, onFlip, onNeverSync, value, onValue }: {
  row: SyncRow; into: string; ticked: boolean; follows: boolean; onFlip: () => void; onNeverSync: () => void;
  value: string; onValue: (value: string) => void;
}) {
  const line = syncLine(row, into);
  const id = `sync-${row.key}`;
  return (
    <li className="grid min-h-10 grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-x-3 border-b last:border-b-0">
      <Checkbox id={id} checked={ticked} disabled={follows} onCheckedChange={onFlip} className={cn(!ticked && "opacity-40")} />
      <label htmlFor={id} className={cn("flex min-w-0 items-center gap-2", !ticked && "opacity-40")}>
        <span className={cn("truncate", line.variable && "font-mono")}>{line.name}</span>
        {line.badge ? <Badge variant={row.changed ? "warning" : "secondary"}>{line.badge}</Badge> : null}
      </label>
      {!ticked && !isWholeNode(row) && !follows ? (
        <Button variant="outline" size="sm" onClick={onNeverSync}><PinIcon data-icon="inline-start" />Never sync</Button>
      ) : row.secret ? (
        <Input type="password" autoComplete="off" aria-label={`Set ${into}'s value of ${line.name}`}
          placeholder={row.value_set ? "Value set" : `Set ${into}'s value`} value={value}
          onChange={(event) => onValue(event.target.value)} className="h-7 w-48 font-mono text-xs" />
      ) : (
        <span className="flex max-w-60 min-w-0 items-center justify-end gap-1.5 font-mono text-xs">
          {line.before ? <><span className="truncate text-muted-foreground">{line.before}</span><ArrowRightIcon className="size-3 shrink-0 text-muted-foreground" /></> : null}
          <span className="truncate">{line.after}</span>
        </span>
      )}
    </li>
  );
}

/** A change never synced: where it's marked, and Sync again to unmark it there. */
function NeverSyncedItem({ row, onSyncAgain }: { row: NeverSyncedRow; onSyncAgain: () => void }) {
  const { name, variable } = syncName(row);
  return (
    <li className="flex min-h-10 items-center gap-3">
      <span className="min-w-0 flex-1 truncate">
        <span className={cn(variable && "font-mono")}>{name}</span>
        <span className="text-muted-foreground"> · {nodeName(row.node)}, marked in {row.marked_in.join(" and ")}</span>
      </span>
      <Button variant="ghost" size="sm" onClick={onSyncAgain}><PinOffIcon data-icon="inline-start" />Sync again</Button>
    </li>
  );
}
