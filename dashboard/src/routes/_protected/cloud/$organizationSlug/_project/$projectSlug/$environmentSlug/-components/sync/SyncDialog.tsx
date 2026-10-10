import { useState } from "react";
import type { EnvironmentRef, NeverSyncedRow, RowId, SyncRow, Synced } from "@ployz/sdk";
import { ArrowRightIcon, ChevronDownIcon, HardDriveIcon, PackageIcon, PinIcon, PinOffIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import { plural } from "#/lib/plural";
import { cn } from "#/lib/utils";
import { nodeName, settingName } from "#/modules/config-store/store-branches";
import { syncLine, syncPicks, syncSections } from "#/modules/config-store/store-sync";
import { syncQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { StoreRefused } from "#/modules/config-store/store.contract";

/**
 * The one review of a Sync: each change `from` would put in `into` as one row, ticked as the Store suggests. Unticked,
 * a change stays out this time and can be marked Never sync; one `into` holds from another source's included change
 * stays unticked until that is removed; the footer lists what is never synced, with undo.
 * Nothing deploys: the changes become `into`'s changes to deploy, or, from a PR Environment into its Destination, go
 * live there when the pull request merges. A secret `into` lacks arrives by name only: its row takes `into`'s own
 * value, set now or held for the merge. Monochrome: pink stays for staged intent and Deploy.
 */
export function SyncDialog({ organizationSlug, from, into, closable, onClose, onSynced }: {
  organizationSlug: string; from: EnvironmentRef; into: string;
  /** Offer to close `from` once synced: a Branch that isn't kept, into its Parent. */
  closable: boolean;
  onClose: () => void;
  /** How many changes synced, and the Sync: what Undo passes, and the Conditional Sync standing for one at the merge. */
  onSynced: (changes: number, synced: Synced) => void;
}) {
  const writer = useStoreWriter(organizationSlug);
  const view = useStoreView(organizationSlug, syncQuery(from, into));
  // Rows the user flipped from their default: they survive a refetch.
  const [flipped, setFlipped] = useState<ReadonlySet<RowId>>(new Set());
  // `into`'s values typed for secrets it lacks: sent with the Sync, never shown back.
  const [values, setValues] = useState<Readonly<Partial<Record<RowId, string>>>>({});
  const [closeAfter, setCloseAfter] = useState(true);
  const [pending, setPending] = useState(false);
  const [stale, setStale] = useState(false);
  // One Sync however often it's sent: a retry after a lost answer is answered with what the first did.
  const [id] = useState(() => crypto.randomUUID());
  // The never-synced list, opened from the footer.
  const [listing, setListing] = useState(false);
  const name = from.environment ?? "";
  const rows = view.ok ? view.value.rows : [];
  const atMerge = view.ok ? view.value.at_merge : null;
  // A PR Environment closes with its pull request.
  const closing = closable && atMerge === null;
  const picked = syncPicks(rows, flipped);
  const pickedRows = new Set(picked.map((row) => row.row));
  const flip = (row: RowId) => setFlipped((current) => {
    const next = new Set(current);
    if (!next.delete(row)) next.add(row);
    return next;
  });
  const neverSync = (environment: string, row: RowId, off: boolean) =>
    writer.commit({ command: "never_sync", environment: { project: from.project, environment }, rows: [row], off });

  async function sync() {
    if (!view.ok) return;
    setPending(true);
    setStale(false);
    // A value lands with the Sync, in one transaction: set now, or held for the merge.
    const typed: Record<RowId, string> = {};
    for (const row of picked) {
      const value = values[row.row];
      if (row.secret && value) typed[row.row] = value;
    }
    let written;
    try {
      // Awaited: the page opens the receiver once it holds the changes, and a stale review stays open, refetched.
      // The Store decides when it lands, as the review read it; only closing after says now.
      written = await writer.commit({
        command: "sync", from, into: { project: from.project, environment: into }, picks: [...pickedRows],
        values: typed, version: view.value.version, id, when: closing && closeAfter ? { kind: "now", close_after: true } : null,
      }, ["conflict"]).isPersisted.promise;
    } catch (error) {
      // Any other refusal is the writer's toast.
      setStale(error instanceof StoreRefused && error.code === "conflict");
      setPending(false);
      return;
    }
    if (written.written === "synced") onSynced(picked.length, written);
  }

  const neverSynced = view.ok ? view.value.never_synced : [];
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent className="flex max-h-[85dvh] flex-col sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Sync to {into}</DialogTitle>
          <DialogDescription>
            {!view.ok ? view.refusal.message : rows.length === 0 ? `Nothing to sync: ${into} has every change from ${name}.`
              : atMerge !== null ? `These changes from ${name} go live in ${into} when #${atMerge} merges.`
              : `These changes from ${name} become ${into}'s changes to deploy.`}
          </DialogDescription>
        </DialogHeader>
        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
          {syncSections(rows).map((section) => (
            <section key={section.node} aria-label={nodeName(section.node)}>
              <h3 className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
                {section.kind === "volume" ? <HardDriveIcon className="size-3.5" /> : <PackageIcon className="size-3.5" />}
                {nodeName(section.node)}
              </h3>
              <ul>
                {section.rows.map((row) => (
                  <SyncRowItem key={row.row} row={row} into={into} ticked={pickedRows.has(row.row)}
                    left={row.requires !== null && !pickedRows.has(row.requires)}
                    onFlip={() => flip(row.row)} onNeverSync={() => neverSync(name, row.row, false)}
                    value={values[row.row] ?? ""} onValue={(value) => setValues((current) => ({ ...current, [row.row]: value }))} />
                ))}
              </ul>
            </section>
          ))}
          {stale ? <p role="status" className="text-muted-foreground">These changed since you opened them. Here they are now.</p> : null}
        </div>
        {listing && neverSynced.length ? (
          <ul id="never-synced" aria-label="Never synced" className="max-h-40 shrink-0 overflow-y-auto border-t">
            {neverSynced.map((row) => <NeverSyncedItem key={row.row} row={row} onSyncAgain={() => {
              for (const mark of row.marks) neverSync(mark.environment, mark.row, true);
            }} />)}
          </ul>
        ) : null}
        {closing && rows.length ? (
          <label className="flex shrink-0 items-center gap-3 border-t pt-4">
            <Checkbox checked={closeAfter} onCheckedChange={setCloseAfter} />
            Close {name} after syncing
          </label>
        ) : null}
        <DialogFooter>
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
 * One change: tick, name (a variable's key in monospace), at most one badge, and old → new on the right; a secret `into`
 * lacks takes `into`'s value there instead. Unticked, it dims and offers Never sync instead. A row of a new node sits
 * under it, and is left out with it (`left`).
 */
function SyncRowItem({ row, into, ticked, left, onFlip, onNeverSync, value, onValue }: {
  row: SyncRow; into: string; ticked: boolean; left: boolean; onFlip: () => void; onNeverSync: () => void;
  value: string; onValue: (value: string) => void;
}) {
  const line = syncLine(row, into);
  const id = `sync-${row.row}`;
  const held = row.held_by !== null;
  return (
    <li className={cn("grid min-h-10 grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-x-3 border-b last:border-b-0",
      row.requires !== null && "pl-6")}>
      <Checkbox id={id} checked={ticked} disabled={left || held} onCheckedChange={onFlip} className={cn(!ticked && "opacity-40")} />
      <label htmlFor={id} className={cn("flex min-w-0 items-center gap-2", !ticked && "opacity-40")}>
        <span className={cn("truncate", line.variable && "font-mono")}>{line.name}</span>
        {line.badge ? <Badge variant={row.change === "conflict" && !row.secret ? "warning" : "secondary"}>{line.badge}</Badge> : null}
      </label>
      {!ticked && row.name !== null && !left && !held ? (
        <Button variant="outline" size="sm" onClick={onNeverSync}><PinIcon data-icon="inline-start" />Never sync</Button>
      ) : row.secret ? (
        <Input type="password" autoComplete="off" aria-label={`Set ${into}'s value of ${line.name}`}
          placeholder={row.secret.held ? "Value held" : `Set ${into}'s value`} value={value}
          onChange={(event) => onValue(event.target.value)} className="ph-no-capture w-48" />
      ) : (
        <span className="ph-no-capture flex max-w-60 min-w-0 items-center justify-end gap-1.5 font-mono text-xs">
          {line.before ? <><span className="truncate text-muted-foreground">{line.before}</span><ArrowRightIcon className="size-3 shrink-0 text-muted-foreground" /></> : null}
          <span className="truncate">{line.after}</span>
        </span>
      )}
    </li>
  );
}

/** A change never synced: where it's marked, and Sync again to unmark it there. */
function NeverSyncedItem({ row, onSyncAgain }: { row: NeverSyncedRow; onSyncAgain: () => void }) {
  const { name, variable } = row.name === null ? { name: nodeName(row.node), variable: false } : settingName(row.name);
  return (
    <li className="flex min-h-10 items-center gap-3">
      <span className="min-w-0 flex-1 truncate">
        <span className={cn(variable && "font-mono")}>{name}</span>
        <span className="text-muted-foreground"> · {nodeName(row.node)}, marked in {[...new Set(row.marks.map((mark) => mark.environment))].join(" and ")}</span>
      </span>
      <Button variant="ghost" size="sm" onClick={onSyncAgain}><PinOffIcon data-icon="inline-start" />Sync again</Button>
    </li>
  );
}
