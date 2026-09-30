import { Handle, Position } from "@xyflow/react";
import { Link, useParams } from "@tanstack/react-router";
import { HardDriveIcon } from "lucide-react";
import type { VolumeListing } from "@ployz/sdk";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Card, CardContent, CardHeader, CardTitle } from "#/components/ui/card";
import { cn } from "#/lib/utils";
import { useNodeLighting } from "../deployment-page";
import { PickedNode } from "./PickableNode";
import { useNodePick } from "../new-branch/branch-picking";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { stagedChip } from "./node-status";
import { DeployChip, StatusLine } from "./service-node-helpers";

const NOT_MOUNTED = { word: "Not mounted", tone: "idle", down: false, since: null } as const;

/** A Volume no Service here mounts, on the canvas and in its phone list: the one kind of Volume that is its own node. */
export function StoreVolumeCard({ volume, selected, className }: { volume: VolumeListing; selected: boolean; className: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const light = useNodeLighting(volume.id);
  const staged = light === undefined && volume.change !== null;
  return (
    <Link
      to={ENVIRONMENT_RESOURCE_ROUTE_TO}
      params={{ ...params, resourceId: volume.id }}
      search={(prev) => ({ ...prev, tab: selected ? prev.tab : undefined })}
      data-canvas-node={volume.id}
      aria-current={selected ? "page" : undefined}
      draggable={false}
      className={cn("rounded-xl", className)}
    >
      <Card size="node" state={staged ? (volume.change === "delete" ? "destructive" : "changed") : undefined}
        className={cn("h-full justify-between", light === null && "opacity-40")} data-selected={selected}>
        <CardHeader>
          <div className="flex items-start gap-3">
            <Avatar><AvatarFallback><HardDriveIcon /></AvatarFallback></Avatar>
            <CardTitle className="min-w-0 flex-1 truncate">{volume.name}</CardTitle>
            <DeployChip light={light} chip={volume.change === null ? null : stagedChip(volume.change, 0)} />
          </div>
        </CardHeader>
        <CardContent><StatusLine status={NOT_MOUNTED} issues={null} /></CardContent>
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
