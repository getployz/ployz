import type { ReactNode } from "react";
import type { ChangeKind, Included, RowId } from "@ployz/sdk";
import { ArrowRightIcon, MinusIcon, MoreVerticalIcon, PencilIcon, PinIcon, PlusIcon, RefreshCwIcon, Undo2Icon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { InputGroup, InputGroupInput } from "#/components/ui/input-group";
import { listNames, plural } from "#/lib/plural";
import { cn } from "#/lib/utils";
import type { ChangeGroup, ChangeRow } from "#/modules/config-store/store-deployments";
import type { Busy } from "#/modules/config-store/store-pending";

/** Where changes came from, said once over them: "From production's deploy", and why they arrived. */
export type ChangeOrigin = { title: string; description: string };

export type EnvironmentChangesReviewProps = {
  environment: string;
  groups: ChangeGroup[];
  totalChanges: number;
  canDeploy: boolean;
  /** Working State differs from Saved State: Save only saves it without deploying. */
  canSave: boolean;
  onSave: () => void;
  onDiscardAll: () => void;
  onClose: () => void;
  onDeploy: () => void;
  /** What the next Deploy ships, in the user's words; shown on its Deployment. */
  message: string;
  onMessageChange: (message: string) => void;
  /** A write the Store hasn't answered for yet. */
  busy?: Busy | null;
  /** Paths of Discards the Store hasn't answered for yet: their lines show Discarding…. */
  discarding?: ReadonlySet<string>;
  onDiscardNode: (group: ChangeGroup) => void;
  onDiscardRow: (group: ChangeGroup, path: string) => void;
  /** A change's second line: the Parent's deployed value, with Use theirs. */
  noteFor?: (row: ChangeRow) => ReactNode;
  /** Never sync for a change that arrived from another Environment: marks it and discards it. */
  neverSyncFor?: (row: ChangeRow) => (() => void) | undefined;
  /** Where the changes in a Sync row (a setting's, or a node's) came from; none for this Environment's own edits. */
  originFor?: (row: RowId) => ChangeOrigin | undefined;
  /** Lists after the changes: the Parent's deployed values no change shows. */
  after?: ReactNode;
  /** The sources Syncs included in this draft. */
  included?: readonly Included[];
  /** Takes a source back out: what it still holds returns to what it was. */
  onRemoveIncluded?: (included: Included) => void;
  /** Reviews the Sync from a source that changed since, to include its newer changes. */
  onIncludeNewer?: (included: Included) => void;
};

/**
 * Details: the changes to deploy in the Sync dialog's anatomy. A group per origin, "Your changes" first; one line per
 * change, coloured by its kind; Discard and Never sync in each line's ⋯; Discard all and Save only in the footer.
 */
export function EnvironmentChangesReview(props: EnvironmentChangesReviewProps) {
  const { environment, canSave, canDeploy, totalChanges, onClose, after, message, onMessageChange, included = [] } = props;
  const staged = canSave || totalChanges > 0 || included.length > 0;
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent className="flex max-h-[85dvh] flex-col sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Environment changes</DialogTitle>
          <DialogDescription>
            {!staged ? "Nothing staged here"
              : `${plural(totalChanges, "change")} in ${environment}, ${canSave ? "not yet saved" : "saved, not deployed"}.`}
          </DialogDescription>
          {staged && (canSave || canDeploy) ? (
            <InputGroup>
              <InputGroupInput aria-label="Change message" placeholder="Message (optional)" maxLength={500} value={message}
                onChange={(event) => onMessageChange(event.target.value)} />
            </InputGroup>
          ) : null}
        </DialogHeader>
        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
          <IncludedList {...props} included={included} />
          {staged ? <ChangeGroups {...props} /> : null}
          {after ? <div className="flex flex-col gap-3">{after}</div> : null}
        </div>
        {staged ? <Footer {...props} /> : null}
      </DialogContent>
    </Dialog>
  );
}

/** One line: a node that is added, removed or updated as a whole, or one of its settings. */
type Line = { group: ChangeGroup; row?: ChangeRow };

/** The lines by where they came from, this Environment's own first, each origin in the order it first appears. */
function changeSections(groups: readonly ChangeGroup[], originFor: EnvironmentChangesReviewProps["originFor"]) {
  const sections = new Map<string, { origin?: ChangeOrigin; lines: Line[] }>([["", { lines: [] }]]);
  for (const group of groups) {
    const node = group.row === null ? undefined : originFor?.(group.row);
    const lines: Line[] = group.lifecycle !== "update" || !group.rows.length ? [{ group }] : [];
    for (const line of [...lines, ...group.rows.map((row) => ({ group, row }))]) {
      // A setting of a node that arrived whole came with it.
      const origin = (line.row?.row && originFor?.(line.row.row)) || node;
      const key = origin?.title ?? "";
      const section = sections.get(key) ?? { origin, lines: [] };
      section.lines.push(line);
      sections.set(key, section);
    }
  }
  return [...sections.values()].filter((section) => section.lines.length);
}

export function ChangeGroups({ groups, onDiscardNode, onDiscardRow, noteFor, neverSyncFor, originFor, discarding }: Pick<EnvironmentChangesReviewProps, "groups" | "noteFor" | "neverSyncFor" | "originFor" | "discarding"> & Partial<Pick<EnvironmentChangesReviewProps, "onDiscardNode" | "onDiscardRow">>) {
  const sections = changeSections(groups, originFor);
  const firstConfigLines = new Map<string, Line>();
  for (const { lines } of sections) {
    for (const line of lines) {
      if (line.group.nodeType === "config" && !firstConfigLines.has(line.group.nodeId)) firstConfigLines.set(line.group.nodeId, line);
    }
  }
  return <>{sections.map(({ origin, lines }) => {
    // Alone, this Environment's own edits need no heading.
    const title = origin?.title ?? (sections.length > 1 ? "Your changes" : undefined);
    return (
      <section key={origin?.title ?? ""} aria-label={title ?? "Changes"}>
        {title ? <h3 className="font-medium">{title}</h3> : null}
        {origin ? <p className="text-muted-foreground">{origin.description}</p> : null}
        <ul>
          {lines.map((line) => {
            const { group, row } = line;
            const firstConfig = firstConfigLines.get(group.nodeId) === line;
            const restarts = firstConfig && group.restarts.length ? (
              <p className="text-muted-foreground">Deploying {group.nodeName} restarts {listNames(group.restarts)}.</p>
            ) : null;
            const discardNode = group.canDiscard && onDiscardNode ? () => onDiscardNode(group) : undefined;
            if (!row) {
              const kind = nodeKinds[group.lifecycle];
              return (
                <ChangeLine key={group.discardPath} kind={kind} label={group.nodeName} pending={discarding?.has(group.discardPath)}
                  value={group.comesBack ? <span className="text-xs text-muted-foreground">Comes back</span> : undefined}
                  onDiscard={firstConfig ? undefined : discardNode}
                  discardNode={firstConfig && discardNode ? { name: group.nodeName, run: discardNode } : undefined}>
                  <p className={cn("truncate", kindText[kind])}>{group.comesBack ? group.nodeName : `${group.nodeName} · ${nodeWords[group.lifecycle]}`}</p>
                  {restarts}
                </ChangeLine>
              );
            }
            const neverSync = neverSyncFor?.(row);
            const note = noteFor?.(row);
            return (
              <ChangeLine key={row.changeKey} kind={row.kind} label={`${group.nodeName} ${row.label}`}
                pending={discarding?.has(row.path) || discarding?.has(group.discardPath)}
                value={row.configFile ? undefined : <Value kind={row.kind} before={row.currentValue} after={row.newValue} />}
                onDiscard={row.canDiscard && onDiscardRow ? () => onDiscardRow(group, row.path) : undefined}
                discardNode={(!row.canDiscard || firstConfig) && discardNode ? { name: group.nodeName, run: discardNode } : undefined}
                onNeverSync={neverSync}>
                <p className="flex min-w-0 items-baseline gap-2">
                  <span className="shrink-0 text-muted-foreground">{group.nodeName}</span>
                  <span className={cn("truncate", row.variable && "font-mono")}>{row.name}</span>
                  {group.unreached ? <span className="shrink-0 text-muted-foreground">Not reached</span> : null}
                </p>
                {row.loss !== undefined ? <p className="text-destructive">{row.loss}</p> : null}
                {row.detail !== undefined ? <p className="text-muted-foreground">{row.detail}</p> : null}
                {row.steps?.length ? <p className="text-muted-foreground">{row.steps.join(" · ")}</p> : null}
                {restarts}
                {row.configFile ? <ConfigFileValue {...row.configFile} /> : null}
                {note ? <div className="text-muted-foreground">{note}</div> : null}
              </ChangeLine>
            );
          })}
        </ul>
      </section>
    );
  })}</>;
}

/**
 * The sources Syncs included here, each with Include newer changes in its ⋯ once it changed, and Remove. Nothing
 * renders for no sources.
 */
function IncludedList({ included, onRemoveIncluded, onIncludeNewer }: Pick<EnvironmentChangesReviewProps, "onRemoveIncluded" | "onIncludeNewer"> & {
  included: readonly Included[];
}) {
  if (!included.length) return null;
  return (
    <section aria-label="Included">
      <h3 className="font-medium">Included</h3>
      <ul>
        {included.map((item) => <IncludedItem key={item.proposal} item={item} onRemove={onRemoveIncluded} onIncludeNewer={onIncludeNewer} />)}
      </ul>
    </section>
  );
}

function IncludedItem({ item, onRemove, onIncludeNewer }: {
  item: Included; onRemove?: (included: Included) => void; onIncludeNewer?: (included: Included) => void;
}) {
  const { source } = item;
  const live = source.kind === "environment" ? source.live : source.environment !== null;
  const newer = item.newer && live && onIncludeNewer ? () => onIncludeNewer(item) : undefined;
  return (
    <li className="grid min-h-10 grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 border-b py-1.5 last:border-b-0">
      <div className="min-w-0">
        <p className="truncate">{source.name} · {plural(item.changes, "change")}</p>
        {item.newer ? <p className="text-muted-foreground">{source.name} changed since</p> : null}
      </div>
      {onRemove ? (
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={`Actions for ${source.name}`} />}>
            <MoreVerticalIcon />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-auto">
            {newer ? <DropdownMenuItem onClick={newer}><RefreshCwIcon />Include newer changes</DropdownMenuItem> : null}
            <DropdownMenuItem variant="destructive" onClick={() => onRemove(item)}><Undo2Icon />Remove</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      ) : <span />}
    </li>
  );
}

const nodeKinds = { create: "add", update: "update", delete: "remove" } as const;
const nodeWords = { create: "will be added", update: "will be updated", delete: "will be removed" } as const;
export const kindText = { add: "text-success", update: "text-changed-deep", remove: "text-destructive" } satisfies Record<ChangeKind, string>;
const kindMarks = {
  add: { icon: <PlusIcon />, badge: "success", word: "Added" },
  update: { icon: <PencilIcon />, badge: "changed", word: "Changed" },
  remove: { icon: <MinusIcon />, badge: "destructive", word: "Removed" },
} as const;

/** One change: its kind's marker, what changed, the value on the right, and ⋯. */
function ChangeLine({ kind, label, value, onDiscard, discardNode, onNeverSync, pending = false, children }: {
  kind: ChangeKind; label: string; value?: ReactNode; onDiscard?: () => void; pending?: boolean | undefined;
  discardNode?: { name: string; run: () => void };
  onNeverSync?: () => void; children: ReactNode;
}) {
  const mark = kindMarks[kind];
  const actions = onDiscard ?? discardNode?.run ?? onNeverSync;
  return (
    <li aria-busy={pending} data-discarding={pending || undefined}
      className={cn("grid min-h-10 grid-cols-[auto_minmax(0,1fr)_auto_auto] items-center gap-x-3 border-b py-1.5 last:border-b-0", pending && "opacity-50")}>
      <Badge variant={mark.badge}>{mark.icon}<span className="sr-only">{mark.word}</span></Badge>
      <div className="min-w-0">{children}</div>
      {value ?? <span />}
      {pending ? <span className="text-xs text-muted-foreground">Discarding…</span> : actions ? (
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={`Actions for ${label}`} />}>
            <MoreVerticalIcon />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-auto">
            {onDiscard ? <DropdownMenuItem onClick={onDiscard}><Undo2Icon />Discard</DropdownMenuItem> : null}
            {discardNode ? <DropdownMenuItem onClick={discardNode.run}><Undo2Icon />Discard all of {discardNode.name}</DropdownMenuItem> : null}
            {onNeverSync ? <DropdownMenuItem onClick={onNeverSync}><PinIcon />Never sync</DropdownMenuItem> : null}
          </DropdownMenuContent>
        </DropdownMenu>
      ) : <span />}
    </li>
  );
}

function ConfigFileValue({ before, after }: NonNullable<ChangeRow["configFile"]>) {
  return (
    <div className="ph-no-capture flex flex-col gap-2 py-2">
      {([{ label: "Before", file: before, other: after, color: "text-destructive" },
        { label: "After", file: after, other: before, color: before ? "text-changed-deep" : "text-success" }]).map(({ label, file, other, color }) => file === null ? null : (
        <div key={label} className="min-w-0">
          <p className="flex flex-wrap gap-x-3 text-xs">
            <span className="text-muted-foreground">{label}</span>
            {(["mode", "uid", "gid"] as const).map((key) => (
              <span key={key} className={cn("font-mono", other && file[key] !== other[key] ? color : "text-muted-foreground")}>
                {key === "mode" ? "Mode" : key.toUpperCase()} {file[key]}
              </span>
            ))}
          </p>
          <pre className={cn("max-h-48 overflow-auto whitespace-pre-wrap break-words font-mono text-xs", color)}>
            {file.content === undefined ? <span className="text-muted-foreground">Hidden</span>
              : file.content === "" ? <span className="text-muted-foreground">Empty file</span> : file.content}
          </pre>
        </div>
      ))}
    </div>
  );
}

/** The value, right-aligned: `old → new` with new in the kind's colour; a removed one struck through. */
export function Value({ kind, before, after }: { kind: ChangeKind; before: string; after: string }) {
  const shown = kind === "remove" ? before : after;
  return (
    <span className="ph-no-capture flex max-w-60 min-w-0 items-center justify-end gap-1.5 font-mono text-xs" title={shown}>
      {kind === "update" && before ? (
        <><span className="truncate text-muted-foreground">{before}</span><ArrowRightIcon className="size-3 shrink-0 text-muted-foreground" /></>
      ) : null}
      <span className={cn("truncate", kindText[kind], kind === "remove" && "line-through")}>{shown}</span>
    </span>
  );
}

/** Discard all, quiet on the left; Save only and Deploy on the right. */
function Footer({ groups, canDeploy, canSave, onSave, onDiscardAll, onDeploy, busy = null }: EnvironmentChangesReviewProps) {
  return (
    <DialogFooter>
      {groups.some((group) => group.canDiscard) ? (
        <Button variant="ghost" className="text-muted-foreground sm:mr-auto" disabled={busy === "discarding"} onClick={onDiscardAll}>{busy === "discarding" ? "Discarding…" : "Discard all"}</Button>
      ) : null}
      <Button variant={canDeploy ? "outline" : "default"} disabled={!canSave || busy === "deploying"} onClick={onSave}>
        {busy === "saving" ? <><Spinner data-icon="inline-start" />Saving…</> : "Save only"}
      </Button>
      {canDeploy ? (
        <Button disabled={busy === "saving" || busy === "deploying"} onClick={onDeploy}>
          {busy === "deploying" ? <><Spinner data-icon="inline-start" />Deploying…</> : "Deploy changes"}
        </Button>
      ) : null}
    </DialogFooter>
  );
}
