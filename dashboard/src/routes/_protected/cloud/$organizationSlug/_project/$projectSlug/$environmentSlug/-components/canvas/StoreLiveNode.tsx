import { Handle, Position } from "@xyflow/react";
import { Link, useParams } from "@tanstack/react-router";
import { Card, CardDescription, CardHeader, CardTitle } from "#/components/ui/card";
import { ENVIRONMENT_LIVE_NODE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { LiveLabel } from "./PickableNode";
import { liveNodeId } from "./nodes";
import type { StoreLiveNode as StoreLive } from "./types";

/** "production's", or why it has no owner. */
export const storeLiveLabel = (live: StoreLive) => live.owner ? `${live.owner}'s` : "Shared, but nothing runs it";

/** A Branch's Live Node over the Config Store: dashed, named for the Environment it runs in. Opens its panel. */
export function StoreLiveNode({ data: { live }, selected }: { data: { live: StoreLive }; selected?: boolean }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  return (
    <Link to={ENVIRONMENT_LIVE_NODE_ROUTE_TO} params={{ ...params, lineageId: live.name }}
      data-canvas-node={liveNodeId(live.name)} draggable={false} className="block h-36 w-72">
      <Handle type="target" position={Position.Bottom} isConnectable={false} className="opacity-0" />
      <Handle type="source" position={Position.Top} isConnectable={false} className="opacity-0" />
      <Card size="node" state="live" className="h-full" data-selected={selected}>
        <CardHeader>
          <CardTitle className="truncate">{live.name}</CardTitle>
          <CardDescription><LiveLabel label={storeLiveLabel(live)} ownsData={live.data} /></CardDescription>
        </CardHeader>
      </Card>
    </Link>
  );
}
