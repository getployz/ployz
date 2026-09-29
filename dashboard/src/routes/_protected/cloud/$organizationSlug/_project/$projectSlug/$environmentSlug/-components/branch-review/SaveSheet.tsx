import { useState, type ReactNode } from "react";
import type { BranchChoice, BranchOption } from "@ployz/sdk";
import { GitBranchIcon, InfoIcon, TriangleAlertIcon, Undo2Icon, XIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldContent, FieldDescription, FieldError, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { plural } from "#/lib/plural";
import type { PresentedRow } from "#/modules/config-store/store-branches";
import { cn } from "#/lib/utils";

type RowPick = { ticked: boolean; option?: BranchOption; value: string };
/**
 * A change as a sheet shows it, from the Cloud document's rows or the Config Store's: `node` adds a whole node, and a
 * `conflict` row changed on the receiving side too.
 */
type SheetRow = { key: string; node: boolean; conflict: boolean; choice?: BranchChoice | undefined };
/** One row of a sheet. Without `pick` it's read-only. */
type Entry = { row: SheetRow; presented: PresentedRow; choice?: BranchChoice | undefined; pick?: RowPick };
type Picks = ReturnType<typeof useRowPicks>;

/**
 * A tick per row and a value choice per variable, starting from core's defaults. `sent` is what the server takes: the
 * ticked rows' keys, options and new values ("" for none). Unticking a new node leaves out its settings too.
 */
export function useRowPicks(rows: Array<{ row: SheetRow; presented: PresentedRow }>) {
  const [edits, setEdits] = useState<Record<string, Partial<RowPick>>>({});
  const picks = rows.map(({ row, presented }) => {
    const pick: RowPick = { ticked: true, option: row.choice?.default, value: "", ...edits[row.key] };
    return { row, choice: row.choice, pick, presented };
  });
  const edit = (key: string, change: Partial<RowPick>) => setEdits((current) => ({ ...current, [key]: { ...current[key], ...change } }));
  const leftOut = new Set(picks.flatMap(({ row, pick, presented }) => row.node && !pick.ticked ? [presented.lineageId] : []));
  const ticked = picks.filter(({ pick, presented }) => pick.ticked && !leftOut.has(presented.lineageId));
  return {
    picks, edit, ticked,
    missing: ticked.find(({ pick }) => pick.option === "new" && !pick.value),
    sent: ticked.map(({ row, pick }) => ({ key: row.key, option: pick.option, value: pick.option === "new" ? pick.value : "" })),
  };
}

/** The sheet: a service per section, a consequence line and its actions. Without `picks` it's read-only. */
export function Sheet({ title, subtitle, entries, picks, destination = "", info, actions, error = null, onClose, children }: {
  title: string; subtitle?: ReactNode; entries: Entry[]; picks?: Picks; destination?: string; info: string; actions: ReactNode;
  error?: string | null; onClose: () => void; children?: ReactNode;
}) {
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          {subtitle}
        </DialogHeader>
        <ServiceSections entries={entries} picks={picks} destination={destination} />
        {children}
        {error ? <FieldError>{error}</FieldError> : null}
        <DialogFooter className="sm:items-center">
          <p className="flex flex-1 items-start gap-2 text-muted-foreground"><InfoIcon className="mt-0.5 size-4 shrink-0" />{info}</p>
          {actions}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** A section per service, in the rows' order. */
function ServiceSections({ entries, picks, destination = "" }: { entries: Entry[]; picks?: Picks; destination?: string }) {
  const groups = new Map<string, Entry[]>();
  for (const entry of entries) groups.set(entry.presented.lineageId, [...(groups.get(entry.presented.lineageId) ?? []), entry]);
  return [...groups].map(([lineage, group]) => <ServiceChanges key={lineage} group={group} picks={picks} destination={destination} />);
}

/** Why Save can't run yet, or null. */
export function saveInfo(rows: Picks, destination: string) {
  return rows.missing ? `Enter ${destination}'s value for ${rows.missing.presented.label}.` : rows.ticked.length === 0 ? "Nothing to save." : null;
}

export function SaveButton({ picks, destination, pending, onClick }: { picks: Picks; destination: string; pending: boolean; onClick: () => void }) {
  return (
    <Button disabled={picks.ticked.length === 0 || picks.missing !== undefined || pending} onClick={onClick}>
      {pending ? <Spinner data-icon="inline-start" /> : <GitBranchIcon data-icon="inline-start" />}Save to {destination}
    </Button>
  );
}

export function SwitchField({ id, label, description, checked, onChange }: {
  id: string; label: string; description?: string; checked: boolean; onChange: (checked: boolean) => void;
}) {
  return (
    <FieldLabel htmlFor={id}>
      <Field orientation="horizontal">
        <FieldContent>{label}{description ? <FieldDescription>{description}</FieldDescription> : null}</FieldContent>
        <Switch id={id} checked={checked} onCheckedChange={onChange} />
      </Field>
    </FieldLabel>
  );
}

/** One service: what happens to it, then its settings as Change · Current · New. A new service's own row is its header's ×. */
function ServiceChanges({ group, picks, destination }: { group: Entry[]; picks: Picks | undefined; destination: string }) {
  const node = group.find(({ row }) => row.node);
  const settings = group.filter((entry) => entry !== node);
  const first = group[0];
  if (!first) return null;
  const leftOut = node?.pick !== undefined && !node.pick.ticked;
  return (
    <section className="flex flex-col gap-2 rounded-lg border p-3" aria-label={first.presented.node}>
      <header className="flex items-center gap-2">
        <span className={cn("flex-1", leftOut && "text-muted-foreground line-through")}>
          <span className="font-medium">{first.presented.node}</span> will be {node ? "added" : "updated"}
        </span>
        {settings.length ? <span className="text-muted-foreground">{plural(settings.length, "setting")}</span> : null}
        {node && picks ? <LeaveOut entry={node} picks={picks} /> : null}
      </header>
      {settings.length && !leftOut ? (
        <div className={cn("grid items-center gap-x-2 gap-y-1", picks
          ? "grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_minmax(0,1fr)_auto]"
          : "grid-cols-2 sm:grid-cols-3")}>
          <span className="hidden text-muted-foreground sm:block">Change</span>
          <span className="text-muted-foreground">Current</span>
          <span className={cn("text-muted-foreground", picks && "col-span-2")}>New</span>
          {settings.map((entry) => <SettingRow key={entry.row.key} entry={entry} picks={picks} destination={destination} />)}
        </div>
      ) : null}
    </section>
  );
}

/** In the Save sheet, a setting the Destination also changed since branching carries a marker. */
function SettingRow({ entry, picks, destination }: { entry: Entry; picks: Picks | undefined; destination: string }) {
  const { presented, pick, row } = entry;
  const off = pick !== undefined && !pick.ticked;
  return (
    <>
      <span className={cn("col-span-full flex flex-wrap items-center gap-1.5 pt-2 font-medium sm:col-span-1 sm:pt-0", off && "text-muted-foreground line-through")}>
        {presented.label}
        {picks && row.conflict ? <Badge variant="warning"><TriangleAlertIcon />Changed in {destination} too</Badge> : null}
      </span>
      <Value className={cn(off && "opacity-50")}>{presented.before || "—"}</Value>
      {picks ? <><NewValue entry={entry} picks={picks} destination={destination} /><LeaveOut entry={entry} picks={picks} /></>
        : <Value>{presented.after || "—"}</Value>}
    </>
  );
}

/** The value that goes to the Destination. A variable's opens to give the Destination its own value. */
function NewValue({ entry: { row, choice, pick, presented }, picks, destination }: { entry: Entry; picks: Picks; destination: string }) {
  const [editing, setEditing] = useState(false);
  if (!pick?.ticked) return <Value className="opacity-50">{presented.after || "—"}</Value>;
  if (choice && pick.option === "new") {
    return (
      <Input className="font-mono" type={choice.secret ? "password" : "text"} autoComplete="off" autoFocus={editing}
        value={pick.value} placeholder="new value" aria-label={`${presented.label} in ${destination}`}
        onChange={(event) => picks.edit(row.key, { value: event.target.value })} />
    );
  }
  const shown = pick.option === "leave_out" ? "—" : presented.after || "—";
  return choice ? (
    <button type="button" className="min-w-0 text-left" title={`Give ${destination} its own value`}
      onClick={() => { setEditing(true); picks.edit(row.key, { option: "new", value: "" }); }}>
      <Value className="border-changed-border bg-changed-soft text-changed-deep">{shown}</Value>
    </button>
  ) : <Value className="border-changed-border bg-changed-soft text-changed-deep">{shown}</Value>;
}

function Value({ className, children }: { className?: string; children: string }) {
  return <span className={cn("block truncate rounded-md border px-2 py-1.5 font-mono", className)} title={children}>{children}</span>;
}

function LeaveOut({ entry: { row, pick, presented }, picks }: { entry: Entry; picks: Picks }) {
  const what = `${presented.node}${presented.label && !row.node ? ` · ${presented.label}` : ""}`;
  const ticked = pick?.ticked ?? true;
  return (
    <Button size="icon-sm" variant="ghost" aria-label={ticked ? `Leave out ${what}` : `Save ${what}`}
      title={ticked ? "Leave out" : "Put back"} onClick={() => picks.edit(row.key, { ticked: !ticked })}>
      {ticked ? <XIcon /> : <Undo2Icon />}
    </Button>
  );
}
