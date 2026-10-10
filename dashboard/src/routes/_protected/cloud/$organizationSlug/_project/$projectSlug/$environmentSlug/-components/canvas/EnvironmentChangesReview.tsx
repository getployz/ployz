import { useState, type ReactNode } from "react";
import type { ChangeKind, Included, RowId } from "@ployz/sdk";
import { ArrowRightIcon, MinusIcon, MoreVerticalIcon, PencilIcon, PinIcon, PlusIcon, RefreshCwIcon, Undo2Icon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Input } from "#/components/ui/input";
import { InputGroup, InputGroupInput } from "#/components/ui/input-group";
import { listNames, plural } from "#/lib/plural";
import { cn } from "#/lib/utils";
import type { ChangeGroup, ChangeRow } from "#/modules/config-store/store-deployments";
import { INCLUDE, needs, readinessLabel, SET_VALUE } from "#/modules/config-store/store-offers";

/** Where changes came from, said once over them: "From production's deploy", and why they arrived. */
export type ChangeOrigin = { title: string; description: string };

export type EnvironmentChangesReviewProps = {
  /** The Environment's name, for the header's line. */
  environment: string;
  groups: ChangeGroup[];
  totalChanges: number;
  canDeploy: boolean;
  /** Working State differs from Saved State: Publish saves it without deploying. */
  canPublish: boolean;
  onPublish: () => void;
  onDiscardAll: () => void;
  onClose: () => void;
  onDeploy: () => void;
  /** What the next Deploy ships, in the user's words; shown on its Deployment. */
  message: string;
  onMessageChange: (message: string) => void;
  /** A Deploy is being admitted: Deploy waits. */
  admitting?: boolean;
  onDiscardNode: (group: ChangeGroup) => void;
  onDiscardRow: (group: ChangeGroup, path: string) => void;
  /** A change's second line: a merged pull request's or the Parent's value, with Use. */
  noteFor?: (row: ChangeRow) => ReactNode;
  /** Never sync for a change that arrived from another Environment: marks it and discards it. */
  neverSyncFor?: (row: ChangeRow) => (() => void) | undefined;
  /** Where the changes in a Sync row (a setting's, or a node's) came from; none for this Environment's own edits. */
  originFor?: (row: RowId) => ChangeOrigin | undefined;
  /** Lists after the changes: merged pull requests' and the Parent's values no change shows. */
  after?: ReactNode;
  /** The sources Syncs included in this draft, and the offers a Merge-menu Sync left to include. */
  included?: readonly Included[];
  /** Takes a source back out, or drops an offer: what it still holds returns to what it was. */
  onRemoveIncluded?: (included: Included) => void;
  /** Moves an offer into the draft, with values for secrets the draft lacks; answers the secrets still needing one. */
  onInclude?: (included: Included, values: Readonly<Record<string, string>>) => Promise<readonly string[]>;
  /** An included pull request that isn't ready: Save and Deploy wait for it. */
  blockedBy?: number | null;
  /** Reviews the Sync from a source that changed since, to include its newer changes. */
  onIncludeNewer?: (included: Included) => void;
};

/**
 * Details: the changes to deploy in the Sync dialog's anatomy. A group per origin, "Your changes" first; one line per
 * change, coloured by its kind; Discard and Never sync in each line's ⋯; Discard all and Publish in the footer.
 */
export function EnvironmentChangesReview(props: EnvironmentChangesReviewProps) {
  const { environment, canPublish, canDeploy, totalChanges, onClose, after, message, onMessageChange, included = [] } = props;
  const staged = canPublish || totalChanges > 0 || included.some((item) => !item.offered);
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent className="flex max-h-[85dvh] flex-col sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Environment changes</DialogTitle>
          <DialogDescription>
            {!staged ? "Nothing staged here"
              : `${plural(totalChanges, "change")} in ${environment}, ${canPublish ? "not yet saved" : "saved, not yet deployed"}.`}
          </DialogDescription>
          {staged && (canPublish || canDeploy) ? (
            <InputGroup>
              <InputGroupInput aria-label="Change message" placeholder="Message (optional)" maxLength={500} value={message}
                onChange={(event) => onMessageChange(event.target.value)} />
            </InputGroup>
          ) : null}
        </DialogHeader>
        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
          {included.length ? <IncludedList {...props} included={included} /> : null}
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
    const node = originFor?.(group.row);
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

export function ChangeGroups({ groups, onDiscardNode, onDiscardRow, noteFor, neverSyncFor, originFor }: Pick<EnvironmentChangesReviewProps, "groups" | "noteFor" | "neverSyncFor" | "originFor"> & Partial<Pick<EnvironmentChangesReviewProps, "onDiscardNode" | "onDiscardRow">>) {
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
                <ChangeLine key={group.discardPath} kind={kind} label={group.nodeName}
                  onDiscard={firstConfig ? undefined : discardNode}
                  discardNode={firstConfig && discardNode ? { name: group.nodeName, run: discardNode } : undefined}>
                  <p className={cn("truncate", kindText[kind])}>{group.nodeName} · {nodeWords[group.lifecycle]}</p>
                  {restarts}
                </ChangeLine>
              );
            }
            const neverSync = neverSyncFor?.(row);
            const note = noteFor?.(row);
            return (
              <ChangeLine key={row.changeKey} kind={row.kind} label={`${group.nodeName} ${row.label}`}
                value={row.configFile ? undefined : <Value kind={row.kind} before={row.currentValue} after={row.newValue} />}
                onDiscard={row.canDiscard && onDiscardRow ? () => onDiscardRow(group, row.path) : undefined}
                discardNode={(!row.canDiscard || firstConfig) && discardNode ? { name: group.nodeName, run: discardNode } : undefined}
                onNeverSync={neverSync}>
                <p className="flex min-w-0 items-baseline gap-2">
                  <span className="shrink-0 text-muted-foreground">{group.nodeName}</span>
                  <span className={cn("truncate", row.variable && "font-mono")}>{row.name}</span>
                </p>
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
 * Each source a Sync included, with Remove and, once it changed, Include newer changes in its ⋯; a pull request's says
 * whether it merged. An offer has Include, which asks here for the value of each secret the draft lacks.
 */
function IncludedList({ included, onRemoveIncluded, onIncludeNewer, onInclude }: Pick<EnvironmentChangesReviewProps, "onRemoveIncluded" | "onIncludeNewer" | "onInclude"> & { included: readonly Included[] }) {
  return (
    <section aria-label="Included">
      <h3 className="font-medium">Included</h3>
      <ul>
        {included.map((item) => <IncludedItem key={item.proposal} item={item} onRemove={onRemoveIncluded}
          onIncludeNewer={onIncludeNewer} onInclude={onInclude} />)}
      </ul>
    </section>
  );
}

function IncludedItem({ item, onRemove, onIncludeNewer, onInclude }: {
  item: Included; onRemove?: (included: Included) => void; onIncludeNewer?: (included: Included) => void;
  onInclude?: EnvironmentChangesReviewProps["onInclude"];
}) {
  const { source } = item;
  // Secrets the last Include named as needing a value, and the values typed for them: sent with the Include, never shown back.
  const [needed, setNeeded] = useState<readonly string[]>([]);
  const [values, setValues] = useState<Readonly<Record<string, string>>>({});
  const [pending, setPending] = useState(false);
  const live = source.kind === "environment" ? source.live : source.environment !== null;
  const newer = item.newer && !item.offered && live && onIncludeNewer ? () => onIncludeNewer(item) : undefined;
  const include = onInclude && item.offered ? async () => {
    setPending(true);
    try {
      setNeeded(await onInclude(item, values));
    } finally {
      setPending(false);
    }
  } : undefined;
  return (
    <li className="grid min-h-10 grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-x-3 border-b py-1.5 last:border-b-0">
      <div className="min-w-0">
        <p className="flex min-w-0 items-center gap-2">
          <span className="truncate">{source.name} · {plural(item.changes, "change")}</span>
          {source.kind === "pull_request" && item.readiness ? (
            <Badge variant={item.readiness === "ready" ? "secondary" : "outline"}>{readinessLabel(item.readiness, source.number)}</Badge>
          ) : null}
        </p>
        {item.newer ? <p className="text-muted-foreground">{source.name} changed since</p> : null}
        {needed.map((name) => (
          <Input key={name} type="password" autoComplete="off" aria-label={`${SET_VALUE} of ${name}`} placeholder={`${SET_VALUE}: ${name}`}
            value={values[name] ?? ""} onChange={(event) => setValues((current) => ({ ...current, [name]: event.target.value }))}
            className="ph-no-capture mt-1.5 w-full" />
        ))}
      </div>
      {include ? <Button size="sm" disabled={pending || needed.some((name) => !values[name])} onClick={() => void include()}>{INCLUDE}</Button> : <span />}
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
const kindText = { add: "text-success", update: "text-changed-deep", remove: "text-destructive" } satisfies Record<ChangeKind, string>;
const kindMarks = {
  add: { icon: <PlusIcon />, badge: "success", word: "Added" },
  update: { icon: <PencilIcon />, badge: "changed", word: "Changed" },
  remove: { icon: <MinusIcon />, badge: "destructive", word: "Removed" },
} as const;

/** One change: its kind's marker, what changed, the value on the right, and ⋯. */
function ChangeLine({ kind, label, value, onDiscard, discardNode, onNeverSync, children }: {
  kind: ChangeKind; label: string; value?: ReactNode; onDiscard?: () => void;
  discardNode?: { name: string; run: () => void };
  onNeverSync?: () => void; children: ReactNode;
}) {
  const mark = kindMarks[kind];
  const actions = onDiscard ?? discardNode?.run ?? onNeverSync;
  return (
    <li className="grid min-h-10 grid-cols-[auto_minmax(0,1fr)_auto_auto] items-center gap-x-3 border-b py-1.5 last:border-b-0">
      <Badge variant={mark.badge}>{mark.icon}<span className="sr-only">{mark.word}</span></Badge>
      <div className="min-w-0">{children}</div>
      {value ?? <span />}
      {actions ? (
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
            {file.content === "" ? <span className="text-muted-foreground">Empty file</span> : file.content}
          </pre>
        </div>
      ))}
    </div>
  );
}

/** The value, right-aligned: `old → new` with new in the kind's colour; a removed one struck through. */
function Value({ kind, before, after }: { kind: ChangeKind; before: string; after: string }) {
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

/** Discard all, quiet on the left; Publish and Deploy on the right, waiting on an included pull request that isn't ready. */
function Footer({ groups, canDeploy, canPublish, onPublish, onDiscardAll, onDeploy, admitting = false, blockedBy = null }: EnvironmentChangesReviewProps) {
  const blocked = blockedBy !== null;
  return (
    <DialogFooter>
      {groups.some((group) => group.canDiscard) ? (
        <Button variant="ghost" className="text-muted-foreground sm:mr-auto" onClick={onDiscardAll}>Discard all</Button>
      ) : null}
      {blocked ? <p role="status" className="self-center text-muted-foreground">{needs(blockedBy)}</p> : null}
      <Button variant={canDeploy ? "outline" : "default"} disabled={!canPublish || blocked} onClick={onPublish}>Save</Button>
      {canDeploy ? <Button disabled={admitting || blocked} onClick={onDeploy}>Deploy changes</Button> : null}
    </DialogFooter>
  );
}
