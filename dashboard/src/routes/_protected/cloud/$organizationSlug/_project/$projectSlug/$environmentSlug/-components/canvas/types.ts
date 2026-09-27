import type { Node } from "@xyflow/react";

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

export type CanvasResourceNode =
  | CanvasServiceNode
  | CanvasVolumeNode;

export type FlowPosition = { x: number; y: number };
export type CreatorPanel = "root" | "git" | "image";
