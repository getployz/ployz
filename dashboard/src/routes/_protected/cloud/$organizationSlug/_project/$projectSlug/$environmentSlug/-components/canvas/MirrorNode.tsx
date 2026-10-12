import { useSyncExternalStore, type ReactNode } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { CloudIcon, HardDriveIcon, TriangleAlertIcon } from "lucide-react";
import type { ChangeKind, ReviewLifecycleKind } from "@ployz/sdk";
import { Badge } from "#/components/ui/badge";
import { Spinner } from "#/components/ui/spinner";
import { cn } from "#/lib/utils";
import {
  cardLine, hangingCopies, type MirrorCopy, type OffsiteCopy, STAGED_WORDS, type StagedCopy, type VolumeCopies,
} from "#/modules/volume-copies/volume-copies";
import { mirrorResourceId } from "#/modules/volume-placement/volume-placement";
import { useNodeLighting } from "../deployment-page";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { stagedChip, stagedSurface } from "./node-status";
import { STAGED_CLASSES } from "./node-status-view";

const LIFECYCLE = { add: "create", update: "update", remove: "delete" } as const satisfies Record<ChangeKind, ReviewLifecycleKind>;
const STAGED_LIFECYCLE = { new: "create", primary: "update", removing: "delete" } as const satisfies Record<StagedCopy, ReviewLifecycleKind>;

/** What the next Deploy does to a mirror, as its chip: New, Primary when it takes over, Removing. */
export function mirrorChip(change: ChangeKind | null) {
  if (change === null) return null;
  const chip = stagedChip(LIFECYCLE[change], 0);
  if (chip.kind !== "staged") return null;
  return { variant: chip.variant, label: change === "update" ? "Primary" : chip.label };
}

const everyMinute = (tick: () => void) => {
  const timer = setInterval(tick, 60_000);
  return () => clearInterval(timer);
};

/** Ages read the clock, which moves between SSR and hydration, so they render only in the browser. */
function useMinute(): Date | null {
  const minute = useSyncExternalStore(everyMinute, () => Math.floor(Date.now() / 60_000), () => null);
  return minute === null ? null : new Date();
}

function CopyCard({ volume, copy, now }: { volume: VolumeCopies["volume"]; copy: MirrorCopy | OffsiteCopy; now: Date | null }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const light = useNodeLighting(volume.id);
  const selectedNodeId = useCanvasInspectorSelection().selectedNodeId;
  const surface = stagedSurface(light, copy.staged === null ? null : STAGED_LIFECYCLE[copy.staged]);
  const line = now === null ? null : cardLine(copy, now);
  const name = copy.kind === "mirror" ? copy.name : copy.bucket;
  const className = cn(
    "relative flex h-14 w-44 shrink-0 flex-col justify-center gap-1 rounded-xl border bg-card px-3 text-xs text-muted-foreground data-[selected=true]:ring-2 data-[selected=true]:ring-foreground",
    "before:absolute before:-top-3.25 before:left-1/2 before:h-3 before:border-l before:border-dashed before:border-muted-foreground/60",
    surface && STAGED_CLASSES[surface].surface,
    light === null && "opacity-40",
  );
  const content: ReactNode = <>
    <span className={cn("flex items-center gap-1.5 text-foreground", surface && "text-inherit")}>
      {copy.kind === "offsite" ? <CloudIcon className="size-3.5 shrink-0" /> : <HardDriveIcon className="size-3.5 shrink-0" />}
      <span className={cn("min-w-0 truncate", surface && STAGED_CLASSES[surface].name)}>{name}</span>
    </span>
    <span className="flex min-w-0 items-center gap-1.5 whitespace-nowrap">
      {line?.spin && <Spinner className="size-3" aria-hidden />}
      {line && <CardWords line={line} />}
      {surface && copy.staged !== null && <Badge variant={surface} className="ml-auto h-4.5 px-1.5">{STAGED_WORDS[copy.staged]}</Badge>}
    </span>
  </>;

  if (copy.kind === "offsite") return <div className={className} data-copy={name}>{content}</div>;
  const resourceId = mirrorResourceId(volume.id, copy.server.id);
  return (
    <Link to={ENVIRONMENT_RESOURCE_ROUTE_TO} params={{ ...params, resourceId }} draggable={false} data-copy={name}
      aria-label={`${name}, mirror of ${volume.name}`} data-selected={selectedNodeId === resourceId} className={className}>
      {content}
    </Link>
  );
}

function CardWords({ line }: { line: NonNullable<ReturnType<typeof cardLine>> }) {
  if (line.age === null) return <span className="min-w-0 truncate">{line.words}</span>;
  if (line.stale) {
    return <>
      <span>{line.words} ·</span>
      <span className="inline-flex min-w-0 items-center gap-1 text-warning">
        <TriangleAlertIcon className="size-3 shrink-0" aria-label="Behind" />
        <span className="truncate">{line.age}</span>
      </span>
    </>;
  }
  return <span className="min-w-0 truncate">{line.words} · {line.age}</span>;
}

/** The copies of the Volumes a node shows, in one row under its trays. */
export function CopyHang({ hang }: { hang: readonly VolumeCopies[] }) {
  const now = useMinute();
  const cards = hang.flatMap((copies) => hangingCopies(copies).map((copy) => ({ volume: copies.volume, copy })));
  if (cards.length === 0) return null;
  return (
    <div className="relative flex justify-center gap-3 pt-6 before:absolute before:top-0 before:left-1/2 before:h-3 before:border-l before:border-dashed before:border-muted-foreground/60">
      {cards.length > 1 && <i className="absolute top-3 right-22 left-22 border-t border-dashed border-muted-foreground/60" />}
      {cards.map(({ volume, copy }) => <CopyCard key={`${volume.id}/${copy.kind}/${copy.id}`} volume={volume} copy={copy} now={now} />)}
    </div>
  );
}
