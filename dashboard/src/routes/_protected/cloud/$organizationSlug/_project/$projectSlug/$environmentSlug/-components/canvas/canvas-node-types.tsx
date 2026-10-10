import type { Node } from "@xyflow/react";
import { Card, CardContent, CardHeader } from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";
import { SERVICE_NODE_HEIGHT, SERVICE_NODE_WIDTH } from "./constants";
import { StoreServiceNode } from "./StoreServiceNode";
import { StoreVolumeNode } from "./StoreVolumeNode";
import { StoreConfigNode } from "./StoreConfigNode";
import { StoreLiveNode } from "./StoreLiveNode";

export const LOADING_NODE: Node<Record<string, never>, "loading"> = {
  id: "loading-placeholder",
  type: "loading",
  position: { x: 0, y: 0 },
  width: SERVICE_NODE_WIDTH,
  height: SERVICE_NODE_HEIGHT,
  data: {},
};

export const canvasNodeTypes = {
  storeService: StoreServiceNode,
  storeVolume: StoreVolumeNode,
  storeConfig: StoreConfigNode,
  storeLive: StoreLiveNode,
  loading: LoadingNode,
};

function LoadingNode() {
  return (
    <Card size="node" className="h-36 w-72 justify-between">
      <CardHeader>
        <div className="flex items-start gap-3">
          <Skeleton className="size-8" />
          <div className="min-w-0 flex-1">
            <Skeleton className="h-5 w-28" />
            <Skeleton className="mt-1.5 h-4 w-36" />
          </div>
        </div>
      </CardHeader>
      <CardContent>
        <div className="flex items-center gap-3">
          <Skeleton className="size-3" />
          <Skeleton className="h-4 w-28" />
        </div>
      </CardContent>
    </Card>
  );
}
