import type { Node } from "@xyflow/react";
import type { AttemptTargetNode, DeploymentNodeView } from "#/modules/deployments/deployment-view";

export type CanvasResourceType = "service" | "volume";

export type CanvasResourceNodeData = {
  resourceType: CanvasResourceType;
  resourceId: string;
  environmentId: string;
};

export type CanvasServiceNodeData = {
  resourceType: "service";
  resourceId: string;
  serviceId: string;
  environmentId: string;
};

export type CanvasServiceNode = Node<CanvasServiceNodeData, "service">;

export type CanvasVolumeNodeData = {
  resourceType: "volume";
  resourceId: string;
  environmentId: string;
};

export type CanvasVolumeNode = Node<CanvasVolumeNodeData, "volume">;

/** A node as a Cloud Deployment Attempt saw it; drawn only in Deployment Mode. */
export type CanvasDeploymentNodeData = {
  /** The node as the attempt's snapshot froze it. */
  node: AttemptTargetNode;
  name: string;
  view: DeploymentNodeView;
};

export type CanvasDeploymentNode = Node<CanvasDeploymentNodeData, "deployment">;

export type CanvasResourceNode =
  | CanvasServiceNode
  | CanvasVolumeNode;

export type FlowPosition = { x: number; y: number };
export type CreatorPanel = "root" | "git" | "image";
