import { Handle, Position } from "@xyflow/react";
import { Link, useParams } from "@tanstack/react-router";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "#/components/ui/card";
import type { LiveNode } from "#/modules/branches/use-live-nodes";
import { useRuntimeService } from "#/providers/runtime-provider";
import { cn } from "#/lib/utils";
import { ENVIRONMENT_LIVE_NODE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { getServiceIcon, getServiceStatusClasses } from "./service-node-helpers";
import { LiveLabel } from "./PickableNode";

/** "production's, live", or why it has no owner. */
export const liveNodeLabel = (liveNode: LiveNode) =>
  liveNode.owner ? `${liveNode.owner.environment.name}'s, live` : "Live, but nothing runs it";

/** How a Live Node is doing where it runs. */
export function useLiveNodeHealth(liveNode: LiveNode) {
  const owner = liveNode.owner;
  const config = owner?.node.nodeType === "service" ? owner.node.config : null;
  const { runtime } = useRuntimeService(owner && config ? `${owner.environment.namespace}/${config.privateDns}` : "");
  const healthy = runtime?.containers.some((container) => container.runtime?.state === "running"
    && (container.runtime.health === "healthy" || container.runtime.health === "not_configured")) ?? false;
  return {
    source: config?.source ?? null,
    healthy,
    text: !runtime ? "No status yet" : healthy ? `Online in ${owner?.environment.name}` : `Not running in ${owner?.environment.name}`,
  };
}

/** A Branch's Live Node: dashed, named for the Environment it comes from, with its health there. Opens its panel. */
export function LiveServiceNode({ data: { liveNode }, selected }: { data: { liveNode: LiveNode }; selected?: boolean }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const health = useLiveNodeHealth(liveNode);
  const status = getServiceStatusClasses(health.healthy ? "success" : undefined);
  return (
    <Link
      to={ENVIRONMENT_LIVE_NODE_ROUTE_TO}
      params={{ ...params, lineageId: liveNode.lineageId }}
      data-canvas-node={`live:${liveNode.lineageId}`}
      draggable={false}
      className="block h-36 w-72"
    >
      <Handle type="target" position={Position.Bottom} isConnectable={false} className="opacity-0" />
      <Handle type="source" position={Position.Top} isConnectable={false} className="opacity-0" />
      <Card size="node" state="live" className="h-full justify-between" data-selected={selected}>
        <CardHeader>
          <div className="flex items-start gap-3">
            <Avatar>
              <AvatarFallback>{health.source ? getServiceIcon({ source: health.source }) : null}</AvatarFallback>
            </Avatar>
            <div className="min-w-0 flex-1 overflow-hidden">
              <CardTitle>{liveNode.name}</CardTitle>
              <CardDescription><LiveLabel label={liveNodeLabel(liveNode)} ownsData={liveNode.ownsData} /></CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <div className="flex items-center gap-3">
            <span className={cn("flex size-3 items-center justify-center rounded-full", status.dot)}>
              <span className={cn("size-1.5 rounded-full", status.innerDot)} />
            </span>
            <span className="min-w-0 flex-1 truncate text-muted-foreground">{health.text}</span>
          </div>
        </CardContent>
      </Card>
    </Link>
  );
}
