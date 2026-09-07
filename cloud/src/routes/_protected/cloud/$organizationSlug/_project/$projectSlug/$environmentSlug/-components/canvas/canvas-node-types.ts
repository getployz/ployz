import type { Node } from "@xyflow/react";
import { SERVICE_NODE_HEIGHT, SERVICE_NODE_WIDTH } from "./constants";
import { LoadingNode, ServiceNode } from "./ServiceNode";
import { VariableGroupNode } from "./VariableGroupNode";
import { VolumeNode } from "./VolumeNode";

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
  variable_group: VariableGroupNode,
  volume: VolumeNode,
  loading: LoadingNode,
};
