import { Handle, Position } from "@xyflow/react";
import { Link, useParams } from "@tanstack/react-router";
import { FolderIcon } from "lucide-react";
import type { ConfigListing } from "@ployz/sdk";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Card, CardContent, CardHeader, CardTitle } from "#/components/ui/card";
import { cn } from "#/lib/utils";
import { useNodeLighting } from "../deployment-page";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { NO_MOUNTS_CONFIGURED, stagedChip, stagedSurface } from "./node-status";
import { DeployChip, STAGED_CLASSES, StatusLine } from "./node-status-view";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";

export function StoreConfigCard({ config, selected, className }: { config: ConfigListing; selected: boolean; className: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const light = useNodeLighting(config.id);
  const surface = stagedSurface(light, config.change);
  return (
    <Link
      to={ENVIRONMENT_RESOURCE_ROUTE_TO}
      params={{ ...params, resourceId: config.id }}
      search={(prev) => ({ ...prev, tab: selected ? prev.tab : undefined })}
      data-canvas-node={config.id}
      aria-current={selected ? "page" : undefined}
      draggable={false}
      preload="intent"
      className={cn("rounded-xl", className)}
    >
      <Card size="node" state={surface}
        className={cn("h-full justify-between", light === null && "opacity-40")} data-selected={selected}>
        <CardHeader>
          <div className="flex items-start gap-3">
            <Avatar><AvatarFallback><FolderIcon /></AvatarFallback></Avatar>
            <CardTitle className={cn("min-w-0 flex-1 truncate", surface && STAGED_CLASSES[surface].name)}>{config.name}</CardTitle>
            <DeployChip light={light} chip={config.change === null ? null : stagedChip(config.change, 0)} />
          </div>
        </CardHeader>
        <CardContent><StatusLine status={NO_MOUNTS_CONFIGURED} issues={null} /></CardContent>
      </Card>
    </Link>
  );
}

export function StoreConfigNode({ data }: { data: { config: ConfigListing } }) {
  const { selectedNodeId } = useCanvasInspectorSelection();
  return (
    <>
      <Handle type="target" position={Position.Bottom} isConnectable={false} className="opacity-0" />
      <Handle type="source" position={Position.Top} isConnectable={false} className="opacity-0" />
      <StoreConfigCard config={data.config} selected={selectedNodeId === data.config.id} className="block h-36 w-72" />
    </>
  );
}
