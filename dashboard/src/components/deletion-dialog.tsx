"use client";

import { useEffect, useEffectEvent, useId, useState, type ReactNode } from "react";
import { GitBranchIcon, HardDriveIcon, LayersIcon, PackageIcon, TriangleAlertIcon } from "lucide-react";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "#/components/ui/alert-dialog";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Field, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import { toErrorMessage } from "#/lib/error-message";

/** One thing a deletion takes with it, drawn with its canvas icon. */
export type DeletionItem = {
  kind: "service" | "volume" | "branch" | "environment" | "project";
  name: string;
  /** Replaces the kind's icon, such as a service's source icon. */
  icon?: ReactNode;
  /** A volume's used bytes, when its server reported them. */
  bytes?: number;
  /** Muted text after the name, such as the server a volume is on. */
  detail?: string;
};

export type DeletionCallbacks = {
  /** Runs when the dialog opens: everything that goes, as the servers see it now. */
  load: () => Promise<readonly DeletionItem[]>;
  /** Starts the deletion. Resolves with the list again when the servers changed, and the dialog asks once more. */
  confirm: () => Promise<void | readonly DeletionItem[]>;
};

export type DeletionDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** The question in the button's words, such as "Delete staging?". */
  title: string;
  /** Where it all is, and what the user types to confirm: "shop/staging". */
  place: string;
  confirmLabel: string;
  /** What Cloud already knows, shown while the servers are asked. */
  items?: readonly DeletionItem[];
  /** Replaces "You're deleting from {place}:". */
  sentence?: ReactNode;
  callbacks: DeletionCallbacks;
};

/** Every thing that goes, by name, and the user types where it is. */
export function DeletionDialog(props: DeletionDialogProps) {
  if (!props.open) return null;
  return <OpenDeletionDialog {...props} />;
}

type Check = { status: "checking" } | { status: "ready" } | { status: "failed"; message: string };

const FAILED = "Something went wrong. Try again.";

function OpenDeletionDialog({ onOpenChange, title, place, confirmLabel, items: known = [], sentence, callbacks }:
  Omit<DeletionDialogProps, "open">) {
  const inputId = useId();
  const [typed, setTyped] = useState("");
  const [items, setItems] = useState(known);
  const [fresh, setFresh] = useState<ReadonlySet<string>>(new Set());
  const [changed, setChanged] = useState(false);
  const [check, setCheck] = useState<Check>({ status: "checking" });
  const [pending, setPending] = useState(false);
  const loadOnOpen = useEffectEvent(callbacks.load);

  useEffect(() => {
    let active = true;
    void loadOnOpen().then(
      (loaded) => {
        if (!active) return;
        setItems(loaded);
        setCheck({ status: "ready" });
      },
      (error) => {
        if (active) setCheck({ status: "failed", message: toErrorMessage(error, FAILED) });
      },
    );
    return () => {
      active = false;
    };
  }, []);

  async function retry() {
    setCheck({ status: "checking" });
    try {
      setItems(await callbacks.load());
      setCheck({ status: "ready" });
    } catch (error) {
      setCheck({ status: "failed", message: toErrorMessage(error, FAILED) });
    }
  }

  async function confirm() {
    if (check.status !== "ready" || typed !== place || pending) return;
    setPending(true);
    try {
      const again = await callbacks.confirm();
      if (!again) {
        onOpenChange(false);
        return;
      }
      const before = new Set(items.map(deletionItemKey));
      setFresh(new Set(again.map(deletionItemKey).filter((key) => !before.has(key))));
      setItems(again);
      setChanged(true);
    } catch (error) {
      setCheck({ status: "failed", message: toErrorMessage(error, FAILED) });
    } finally {
      setPending(false);
    }
  }

  return (
    <AlertDialog open onOpenChange={onOpenChange}>
      <AlertDialogContent
        render={<form onSubmit={(event) => { event.preventDefault(); void confirm(); }} />}
        className="sm:max-w-md"
      >
        <AlertDialogHeader>
          <AlertDialogTitle className="flex items-center gap-2">
            <TriangleAlertIcon className="size-4 text-destructive" />
            {title}
          </AlertDialogTitle>
          <AlertDialogDescription>
            {sentence ?? <>You're <span className="text-destructive">deleting</span> from <span className="text-foreground">{place}</span>:</>}
          </AlertDialogDescription>
          {check.status === "failed" ? (
            <p className="text-sm text-destructive">
              {check.message}{" "}
              <Button type="button" variant="link" className="h-auto p-0 text-destructive underline" onClick={() => void retry()}>
                Retry
              </Button>
            </p>
          ) : null}
          {changed && fresh.size === 0 ? (
            <p className="text-sm text-warning">Your servers changed since you opened this.</p>
          ) : null}
        </AlertDialogHeader>
        {items.length > 0 ? <DeletionList items={items} fresh={fresh} /> : null}
        <Field>
          <FieldLabel htmlFor={inputId} className="font-normal">
            Type <strong className="font-mono font-medium">{place}</strong> to confirm
          </FieldLabel>
          <Input id={inputId} autoFocus autoComplete="off" spellCheck={false} placeholder={place} value={typed}
            onChange={(event) => setTyped(event.target.value)} />
        </Field>
        <AlertDialogFooter>
          <AlertDialogCancel type="button" disabled={pending}>Cancel</AlertDialogCancel>
          <AlertDialogAction type="submit" variant="destructive" disabled={check.status !== "ready" || typed !== place || pending}>
            {pending ? <Spinner data-icon="inline-start" /> : null}
            {confirmLabel}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

const KIND_ICON = {
  service: <PackageIcon />,
  volume: <HardDriveIcon />,
  branch: <GitBranchIcon />,
  environment: <LayersIcon />,
  project: <LayersIcon />,
} satisfies Record<DeletionItem["kind"], ReactNode>;

export function deletionItemKey(item: DeletionItem) {
  return `${item.kind}\0${item.name}\0${item.detail ?? ""}`;
}

export type DeletionRow =
  | { type: "item"; item: DeletionItem; isNew: boolean }
  | { type: "group"; kind: DeletionItem["kind"]; label: string; members: readonly DeletionItem[]; bytes?: number };

const SHOW_ALL_UP_TO = 4;
const NAMED_VOLUMES = 2;
const GROUPED_KINDS = ["service", "branch", "environment", "project"] as const;

const plural = (count: number, word: string) =>
  `${count} ${word}${count === 1 ? "" : word.endsWith("ch") ? "es" : "s"}`;

/**
 * Up to four things are all named. Past that, name what can't come back (anything new, then the biggest volumes) and
 * count the rest, one row per kind.
 */
export function deletionRows(items: readonly DeletionItem[], fresh: ReadonlySet<string>): DeletionRow[] {
  const isNew = (item: DeletionItem) => fresh.has(deletionItemKey(item));
  const named = (list: readonly DeletionItem[]): DeletionRow[] =>
    list.map((item) => ({ type: "item", item, isNew: isNew(item) }));
  const volumes = items.filter((item) => !isNew(item) && item.kind === "volume")
    .sort((a, b) => (b.bytes ?? -1) - (a.bytes ?? -1));
  const others = GROUPED_KINDS.map((kind) => ({ kind, members: items.filter((item) => !isNew(item) && item.kind === kind) }));
  if (items.length <= SHOW_ALL_UP_TO) {
    return named([...items.filter(isNew), ...volumes, ...others.flatMap(({ members }) => members)]);
  }
  const rest = volumes.slice(NAMED_VOLUMES);
  const restBytes = rest.every((item) => item.bytes !== undefined)
    ? rest.reduce((sum, item) => sum + (item.bytes ?? 0), 0)
    : undefined;
  return [
    ...named(items.filter(isNew)),
    ...named(volumes.slice(0, NAMED_VOLUMES)),
    ...(rest.length > 0
      ? [{ type: "group" as const, kind: "volume" as const, label: `${rest.length} more ${rest.length === 1 ? "volume" : "volumes"}`, members: rest, bytes: restBytes }]
      : []),
    ...others.flatMap(({ kind, members }): DeletionRow[] =>
      members.length > 0 ? [{ type: "group", kind, label: plural(members.length, kind), members }] : []),
  ];
}

function DeletionList({ items, fresh }: { items: readonly DeletionItem[]; fresh: ReadonlySet<string> }) {
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  return (
    <ul className="flex max-h-72 flex-col gap-2.5 overflow-y-auto rounded-lg border p-3">
      {deletionRows(items, fresh).flatMap((row) => {
        if (row.type === "item") return [<ItemRow key={deletionItemKey(row.item)} item={row.item} isNew={row.isNew} />];
        const shown = open.has(row.kind);
        return [
          <li key={`group:${row.kind}`} className="flex items-center gap-3 text-sm">
            <span className="flex text-muted-foreground [&_svg]:size-4">{KIND_ICON[row.kind]}</span>
            <span className="min-w-0 flex-1 truncate">{row.label}</span>
            {row.bytes !== undefined ? <span className="font-mono text-muted-foreground">{formatBytes(row.bytes)}</span> : null}
            <Button type="button" variant="link" className="h-auto p-0 text-muted-foreground" aria-expanded={shown}
              onClick={() => setOpen((current) => {
                const next = new Set(current);
                if (shown) next.delete(row.kind);
                else next.add(row.kind);
                return next;
              })}>
              {shown ? "Hide" : "Show"}
            </Button>
          </li>,
          ...(shown ? row.members.map((item) => <ItemRow key={deletionItemKey(item)} item={item} isNew={false} />) : []),
        ];
      })}
    </ul>
  );
}

function ItemRow({ item, isNew }: { item: DeletionItem; isNew: boolean }) {
  return (
    <li className="flex items-center gap-3 text-sm">
      <span className="flex text-muted-foreground [&_svg]:size-4">{item.icon ?? KIND_ICON[item.kind]}</span>
      <span className="min-w-0 flex-1 truncate font-mono">{item.name}</span>
      {isNew ? <Badge variant="outline" className="border-warning-border bg-warning-soft text-warning-foreground">New</Badge> : null}
      {item.detail ? <span className="text-muted-foreground">{item.detail}</span> : null}
      {item.bytes !== undefined ? <span className="font-mono text-muted-foreground">{formatBytes(item.bytes)}</span> : null}
    </li>
  );
}

const BYTE_UNITS = ["B", "KB", "MB", "GB", "TB"] as const;

export function formatBytes(bytes: number) {
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${new Intl.NumberFormat("en-US", { maximumFractionDigits: unit > 0 && value < 100 ? 1 : 0 }).format(value)} ${BYTE_UNITS[unit]}`;
}
