import { Link, useParams } from "@tanstack/react-router";
import { HardDriveIcon } from "lucide-react";
import type { ChangeKind, ReviewLifecycleKind } from "@ployz/sdk";
import { Badge } from "#/components/ui/badge";
import { RelativeTime } from "#/components/relative-time";
import { cn } from "#/lib/utils";
import { mirrorResourceId, type CanvasMirror } from "#/modules/volume-placement/volume-placement";
import { copyName } from "#/modules/volume-run/volume-run";
import { useNodeLighting } from "../deployment-page";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { stagedChip, stagedSurface } from "./node-status";
import { STAGED_CLASSES } from "./node-status-view";

const LIFECYCLE = { add: "create", update: "update", remove: "delete" } as const satisfies Record<ChangeKind, ReviewLifecycleKind>;

/** What the next Deploy does to a mirror, as its chip: New, Primary when it takes over, Removing. */
export function mirrorChip(change: ChangeKind | null) {
  if (change === null) return null;
  const chip = stagedChip(LIFECYCLE[change], 0);
  if (chip.kind !== "staged") return null;
  return { variant: chip.variant, label: change === "update" ? "Primary" : chip.label };
}

/** A Volume's mirror, hung by a dashed line under the tray or card that shows the Volume. It opens the mirror's own drawer. */
export function MirrorNode({ mirror: { volume, server, change, newest } }: { mirror: CanvasMirror }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const light = useNodeLighting(volume.id);
  const lifecycle = change === null ? null : LIFECYCLE[change];
  const surface = stagedSurface(light, lifecycle);
  const chip = surface ? mirrorChip(change) : null;
  const name = copyName(volume.name, server.name);
  const resourceId = mirrorResourceId(volume.id, server.id);
  const selected = useCanvasInspectorSelection().selectedNodeId === resourceId;
  return (
    <div className={cn("flex flex-col items-center", light === null && "opacity-40")} data-mirror={name}>
      <div className="h-6 border-l border-dashed border-muted-foreground/60" />
      <Link to={ENVIRONMENT_RESOURCE_ROUTE_TO} params={{ ...params, resourceId }} draggable={false}
        aria-label={`${name}, mirror of ${volume.name}`} data-selected={selected}
        className={cn("flex h-14 w-50 flex-col justify-center gap-1 rounded-xl border bg-card px-3 text-xs text-muted-foreground data-[selected=true]:ring-2 data-[selected=true]:ring-foreground",
          surface && STAGED_CLASSES[surface].surface)}>
        <span className="flex items-center gap-2 text-foreground">
          <HardDriveIcon className="size-3.5 shrink-0" />
          <span className={cn("min-w-0 truncate", surface && STAGED_CLASSES[surface].name)}>{name}</span>
        </span>
        <span className="flex items-center gap-2">
          <span className="min-w-0 flex-1 truncate">
            {change === "remove" ? null : "Mirror"}
            {newest && change !== "add" && <>{change === "remove" ? null : " · "}<RelativeTime date={newest} /></>}
          </span>
          {chip && <Badge variant={chip.variant}>{chip.label}</Badge>}
        </span>
      </Link>
    </div>
  );
}

export function MirrorNodes({ mirrors }: { mirrors: readonly CanvasMirror[] }) {
  return mirrors.map((mirror) => <MirrorNode key={`${mirror.volume.id}/${mirror.server.id}`} mirror={mirror} />);
}
