import type { Node } from "@xyflow/react";
import { SERVICE_NODE_HEIGHT, SERVICE_NODE_WIDTH } from "./constants";
import { LoadingNode, ServiceNode } from "./ServiceNode";
import { VolumeNode } from "./VolumeNode";
import { StoreServiceNode } from "./StoreServiceNode";
import { LiveServiceNode } from "./LiveServiceNode";

export const LOADING_NODE: Node<Record<string, never>, "loading"> = {
  id: "loading-placeholder",
  type: "loading",
  position: { x: 0, y: 0 },
  width: SERVICE_NODE_WIDTH,
  height: SERVICE_NODE_HEIGHT,
  data: {},
};

export const canvasNodeTypes = {
  service: ServiceNode,
  storeService: StoreServiceNode,
  volume: VolumeNode,
  live: LiveServiceNode,
  loading: LoadingNode,
};
