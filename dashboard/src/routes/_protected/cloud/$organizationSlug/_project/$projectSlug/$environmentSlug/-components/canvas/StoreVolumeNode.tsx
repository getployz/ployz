import { Handle, Position } from "@xyflow/react";
import { Link, useParams } from "@tanstack/react-router";
import { HardDriveIcon } from "lucide-react";
import type { VolumeListing } from "@ployz/sdk";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Badge } from "#/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "#/components/ui/card";
import { outcomeCardState } from "#/components/deployment-outcome-badges";
import { cn } from "#/lib/utils";
import { volumeStorageText } from "#/modules/config-store/store-volumes";
import { useNodeLighting } from "../deployment-page";
import { NodeOutcomeBadge } from "./NodeOutcomeBadge";
import { PickedNode } from "./PickableNode";
import { useNodePick } from "../new-branch/branch-picking";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/** A Config Store Volume on the canvas and in its phone list: its mount paths, and what the next Deploy does to it. */
export function StoreVolumeCard({ volume, selected, className }: { volume: VolumeListing; selected: boolean; className: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const paths = volume.mounts.map((mount) => mount.path);
  const summary = paths.length === 0 ? "No mounts" : paths.length > 2 ? `${paths.slice(0, 2).join(", ")}, +${paths.length - 2}` : paths.join(", ");
  const removing = volume.change === "delete";
  const light = useNodeLighting(volume.id);
  return (
    <Link
      to={ENVIRONMENT_RESOURCE_ROUTE_TO}
      params={{ ...params, resourceId: volume.id }}
      search={(prev) => ({ ...prev, tab: selected ? prev.tab : undefined })}
      data-canvas-node={volume.id}
      aria-current={selected ? "page" : undefined}
      draggable={false}
      className={className}
    >
      <Card size="node" state={light ? outcomeCardState(light.outcome) : removing ? "destructive" : volume.change === "create" ? "success" : undefined}
        className={cn("h-full justify-between", light === null && "opacity-40")} data-selected={selected}>
        <CardHeader>
          <div className="flex items-start gap-3">
            <Avatar><AvatarFallback><HardDriveIcon /></AvatarFallback></Avatar>
            <div className="min-w-0 flex-1 overflow-hidden">
              <CardTitle className="truncate">{volume.name}</CardTitle>
              <CardDescription>Volume</CardDescription>
            </div>
            {light ? <NodeOutcomeBadge light={light} /> : removing ? <Badge variant="destructive">Removing</Badge> : volume.change === "create" ? <Badge variant="success">New</Badge> : null}
          </div>
        </CardHeader>
        <CardContent>
          <p className="truncate text-muted-foreground">{volumeStorageText(volume.storage)}</p>
          <p className="truncate text-muted-foreground" title={paths.join(", ")}>{summary}</p>
        </CardContent>
      </Card>
    </Link>
  );
}

export function StoreVolumeNode({ data, selected }: { data: { volume: VolumeListing }; selected?: boolean }) {
  const pick = useNodePick(data.volume.name);
  if (pick) return <PickedNode pick={pick} name={data.volume.name} nodeId={data.volume.id} icon={<HardDriveIcon />} />;
  return (
    <>
      <Handle type="target" position={Position.Bottom} isConnectable={false} className="opacity-0" />
      <Handle type="source" position={Position.Top} isConnectable={false} className="opacity-0" />
      <StoreVolumeCard volume={data.volume} selected={selected ?? false} className="block h-36 w-72" />
    </>
  );
}
