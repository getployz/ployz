import { useRef, useState, useTransition } from "react";
import type { ConfigCommand, EnvironmentRef, NeverSyncedRow, RowId, SecretValue, SyncRow, Synced } from "@ployz/sdk";
import { ArrowRightIcon, ChevronDownIcon, HardDriveIcon, PackageIcon, PinIcon, PinOffIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Input } from "#/components/ui/input";
import { NativeSelect, NativeSelectOption } from "#/components/ui/native-select";
import { Spinner } from "#/components/ui/spinner";
import { plural } from "#/lib/plural";
import { cn } from "#/lib/utils";
import { nodeName, settingName } from "#/modules/config-store/store-branches";
import { useStoreBusy, useStoreNeverSyncs } from "#/modules/config-store/store-pending";
import { flipRow, flipsMoved, showNeverSyncing, syncLine, syncPicks, syncSections, type Flips } from "#/modules/config-store/store-sync";
import { fetchStoreView, servicesQuery, syncQuery, useCachedStoreView, useStoreView } from "#/modules/config-store/store-view.queries";
import { receivingService, receivingValues } from "#/modules/config-store/store-variables";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useSealing } from "#/modules/variables/sealing";
import { isRequested, useStoreWriter } from "#/modules/config-store/store-write";
import { useProtectedAsk } from "#/modules/config-store/use-protected-ask";
import { StoreRefused } from "#/modules/config-store/store.contract";

/**
 * The one review of a Sync, "Make into match from": each change `from` would put in `into` as one row, ticked as the Store suggests. Unticked,
 * a change stays out this time and can be marked Never sync; the footer lists what is never synced, with undo.
 * Nothing deploys: the changes become `into`'s changes to deploy. A secret `into` lacks arrives Not set: its row takes
 * `into`'s own value. Monochrome: pink stays for staged intent and Deploy. With `places`, `into` is a picker over them.
 */
export function SyncDialog({ organizationSlug, from, into: first, places = [], closable, onClose, onSynced }: {
  organizationSlug: string; from: EnvironmentRef; into: string;
  /** The places the user can pick `into` from. */
  places?: readonly string[];
  /** Offer to close `from` once synced into there: a Branch that isn't kept, into its Parent. */
  closable: (into: string) => boolean;
  onClose: () => void;
  /** Where it synced, how many changes, and the Sync: what Undo passes. */
  onSynced: (into: string, changes: number, synced: Synced) => void;
}) {
  const writer = useStoreWriter(organizationSlug);
  const scope = useCollectionScope();
  const sealing = useSealing(organizationSlug);
  const [into, setInto] = useState(first);
  // The open review stays up while the picked place's loads.
  const [, startTransition] = useTransition();
  const read = useStoreView(organizationSlug, syncQuery(from, into));
  // A Never sync shows at once: its row leaves the changes for the list, or back.
  const neverSyncs = useStoreNeverSyncs(organizationSlug, from.project);
  const view = read.ok && neverSyncs.length ? { ...read, value: showNeverSyncing(read.value, neverSyncs) } : read;
  // Rows the user flipped from their default: a flip survives a refetch until its row changes.
  const [flips, setFlips] = useState<Flips>(new Map());
  // `into`'s values typed for secrets it lacks: sent with the Sync, never shown back.
  const [values, setValues] = useState<Readonly<Partial<Record<RowId, string>>>>({});
  const [closeAfter, setCloseAfter] = useState(true);
  const pending = useStoreBusy(organizationSlug, from).has("syncing");
  const asks = useProtectedAsk(organizationSlug, { project: from.project, environment: into });
  const [stale, setStale] = useState(false);
  // The id of the last Sync sent and what it asked: an identical retry reuses it, so a lost response never syncs twice,
  // and changed picks, values or version mint a new one, so the Store never answers them with the old Sync.
  // What it sent is kept with the key it sealed to: a retry resends it only while a fresh read finds that key still, and
  // else prepares afresh under the same id, so a Sync that landed is answered, not rerun.
  const lastSync = useRef<{ intent: string; id: string; sent?: { under: string; command: Promise<ConfigCommand> } } | null>(null);
  const [neverSyncedOpen, setNeverSyncedOpen] = useState(false);
  const name = from.environment ?? "";
  const rows = view.ok ? view.value.rows : [];
  const picked = syncPicks(rows, flips);
  const pickedRows = new Set(picked.map((row) => row.row));
  // Only while sealing shows ready does the dialog watch `into`'s Services, to hold a value for a Service new there.
  const receiver = useCachedStoreView(organizationSlug, sealing.state === "ready" ? servicesQuery({ project: from.project, environment: into }) : null);
  const homeless = (row: SyncRow) => receiver?.ok === true && receivingService(row.row, receiver.value.services) === undefined;
  const neverSync = (environment: string, row: RowId, off: boolean) =>
    writer.commit({ command: "never_sync", environment: { project: from.project, environment }, rows: [row], off });

  async function sync() {
    if (!view.ok) return;
    setStale(false);
    // A value lands with the Sync, in one transaction.
    const typed: Record<RowId, string> = {};
    for (const row of picked) {
      const value = values[row.row];
      if (row.secret && value && !homeless(row)) typed[row.row] = value;
    }
    const closeAfterSync = closable(into) && closeAfter;
    const intent = JSON.stringify([into, [...pickedRows], typed, view.value.version, closeAfterSync]);
    if (lastSync.current?.intent !== intent) lastSync.current = { intent, id: crypto.randomUUID() };
    const last = lastSync.current;
    const receiving = { project: from.project, environment: into };
    const command = (sent: Record<RowId, SecretValue>): ConfigCommand => ({
      command: "sync", from, into: receiving, picks: [...pickedRows],
      values: sent, version: view.value.version, id: last.id, close_after: closeAfterSync,
    });
    const entries = picked.flatMap((row) => typed[row.row] === undefined ? [] : [{ row: row.row, value: typed[row.row] ?? "" }]);
    // Sealed in its turn in the queue.
    const prepare = async () => {
      const seal = await sealing.sealer();
      const under = seal.publicKey;
      if (last.sent?.under !== under) {
        last.sent = { under, command: receivingValues({ sealer: async () => seal }, entries, async () => {
          const services = await fetchStoreView(organizationSlug, scope, servicesQuery(receiving));
          return { environment: services.environment.id, services: services.services };
        }, "drop").then((sent) => command(Object.fromEntries(sent.map((one) => [one.row, one.value])))) };
      }
      try {
        return await last.sent.command;
      } catch (error) {
        // A refused seal sends nothing, so the next try seals afresh.
        last.sent = undefined;
        throw error;
      }
    };
    let written;
    try {
      // Awaited: the page opens the receiver once it holds the changes, and a stale review stays open, refetched.
      written = await writer.commit(entries.length ? command({}) : command(typed), ["conflict"], entries.length ? prepare : undefined).isPersisted.promise;
    } catch (error) {
      // Asked for instead: the writer says Requested, and an owner or admin syncs it on approval.
      if (error instanceof Error && isRequested(error)) return onClose();
      // Only a lost response resends what was sent; after a refusal the next try prepares afresh.
      if (error instanceof StoreRefused) last.sent = undefined;
      // Any other refusal is the writer's toast.
      setStale(error instanceof StoreRefused && error.code === "conflict");
      return;
    }
    if (written.written === "synced") onSynced(into, picked.length, written);
  }

  const neverSynced = view.ok ? view.value.never_synced : [];
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent className="flex max-h-[85dvh] flex-col sm:max-w-xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            Make {places.length > 1 ? (
              <NativeSelect size="sm" aria-label="Place to sync into" value={into} disabled={pending} onChange={(event) => {
                const picked = event.target.value;
                startTransition(() => { setInto(picked); setFlips(new Map()); setValues({}); });
              }}>
                {places.map((place) => <NativeSelectOption key={place} value={place}>{place}</NativeSelectOption>)}
              </NativeSelect>
            ) : into} match {name}
          </DialogTitle>
          <DialogDescription>
            {!view.ok ? view.refusal.message : rows.length === 0 ? "Already the same." : `These become ${into}'s changes to deploy.`}
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
                    onFlip={() => setFlips((current) => flipRow(current, row))} onNeverSync={() => neverSync(name, row.row, false)}
                    homeless={homeless(row)} value={values[row.row] ?? ""} onValue={(value) => setValues((current) => ({ ...current, [row.row]: value }))} />
                ))}
              </ul>
            </section>
          ))}
          {stale || flipsMoved(rows, flips) ? <p role="status" className="text-muted-foreground">These changed since you opened them. Here they are now.</p> : null}
        </div>
        {neverSyncedOpen && neverSynced.length ? (
          <ul id="never-synced" aria-label="Never synced" className="max-h-40 shrink-0 overflow-y-auto border-t">
            {neverSynced.map((row) => <NeverSyncedItem key={row.row} row={row} onSyncAgain={() => {
              for (const mark of row.marks) neverSync(mark.environment, mark.row, true);
            }} />)}
          </ul>
        ) : null}
        {closable(into) && rows.length ? (
          <label className="flex shrink-0 items-center gap-3 border-t pt-4">
            <Checkbox checked={closeAfter} onCheckedChange={setCloseAfter} />
            Close {name} after syncing
          </label>
        ) : null}
        <DialogFooter>
          {neverSynced.length ? (
            <Button variant="ghost" className="text-muted-foreground sm:mr-auto" aria-expanded={neverSyncedOpen} aria-controls="never-synced"
              onClick={() => setNeverSyncedOpen(!neverSyncedOpen)}>
              {neverSynced.length} never synced<ChevronDownIcon data-icon="inline-end" className={cn(neverSyncedOpen && "rotate-180")} />
            </Button>
          ) : null}
          {rows.length ? <>
            <Button variant="outline" onClick={onClose}>Cancel</Button>
            <Button disabled={picked.length === 0 || pending} onClick={() => void sync()}>
              {pending ? <><Spinner data-icon="inline-start" />Syncing…</> : asks ? "Request sync" : `Sync ${plural(picked.length, "change")}`}
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
function SyncRowItem({ row, into, ticked, left, onFlip, onNeverSync, homeless, value, onValue }: {
  row: SyncRow; into: string; ticked: boolean; left: boolean; onFlip: () => void; onNeverSync: () => void;
  /** Its Service is new in `into`, so a sealed value can't go with it. */
  homeless: boolean;
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
      ) : row.secret && homeless ? (
        <span className="text-xs text-muted-foreground">Set this value after syncing.</span>
      ) : row.secret ? (
        <Input type="password" autoComplete="off" aria-label={`Set ${into}'s value of ${line.name}`}
          placeholder={`Set ${into}'s value`} value={value}
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
