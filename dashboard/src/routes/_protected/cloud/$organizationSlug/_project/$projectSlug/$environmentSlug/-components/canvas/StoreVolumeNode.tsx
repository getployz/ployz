import { Handle, Position } from "@xyflow/react";
import { Link, useParams } from "@tanstack/react-router";
import { HardDriveIcon } from "lucide-react";
import type { VolumeListing } from "@ployz/sdk";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Card, CardContent, CardHeader, CardTitle } from "#/components/ui/card";
import { Progress } from "#/components/ui/progress";
import { cn } from "#/lib/utils";
import { useNodeLighting } from "../deployment-page";
import { PickedNode } from "./PickableNode";
import { useNodePick } from "../new-branch/branch-picking";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { NOT_MOUNTED, fillText, fillTone, nodeIssues, stagedChip, stagedSurface } from "./node-status";
import { DeployChip, FILL_CLASSES, STAGED_CLASSES, StatusLine } from "./node-status-view";
import { useVolumeFill } from "./RuntimeLensProvider";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";

/**
 * A Volume no Service here mounts, on the canvas and in its phone list: the one kind of Volume that is its own node. It
 * shows how full it is, when its Servers say.
 */
export function StoreVolumeCard({ volume, selected, className }: { volume: VolumeListing; selected: boolean; className: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const light = useNodeLighting(volume.id);
  const surface = stagedSurface(light, volume.change);
  const fill = useVolumeFill()(volume.id);
  const tone = FILL_CLASSES[fillTone(fill) ?? "ok"];
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
      <Card size="node" state={surface}
        className={cn("h-full justify-between", light === null && "opacity-40")} data-selected={selected}>
        <CardHeader>
          <div className="flex items-start gap-3">
            <Avatar><AvatarFallback><HardDriveIcon /></AvatarFallback></Avatar>
            <CardTitle className={cn("min-w-0 flex-1 truncate", surface && STAGED_CLASSES[surface].name)}>{volume.name}</CardTitle>
            <DeployChip light={light} chip={volume.change === null ? null : stagedChip(volume.change, 0)} />
          </div>
        </CardHeader>
        <CardContent className="flex flex-col gap-2">
          {fill === null ? null : (
            <div className={cn("flex items-center gap-2 text-xs", tone.text)}>
              <Progress value={fill * 100} aria-label="Fill" className="flex-1 **:data-[slot=progress-indicator]:bg-current" />
              {fillText(fill)}
            </div>
          )}
          <StatusLine status={NOT_MOUNTED} issues={nodeIssues(NOT_MOUNTED, [], [{ volume, fill }])} />
        </CardContent>
      </Card>
    </Link>
  );
}

export function StoreVolumeNode({ data }: { data: { volume: VolumeListing } }) {
  const pick = useNodePick(data.volume.name);
  const { selectedNodeId } = useCanvasInspectorSelection();
  if (pick) return <PickedNode pick={pick} name={data.volume.name} nodeId={data.volume.id} icon={<HardDriveIcon />} />;
  return (
    <>
      <Handle type="target" position={Position.Bottom} isConnectable={false} className="opacity-0" />
      <Handle type="source" position={Position.Top} isConnectable={false} className="opacity-0" />
      <StoreVolumeCard volume={data.volume} selected={selectedNodeId === data.volume.id} className="block h-36 w-72" />
    </>
  );
}
