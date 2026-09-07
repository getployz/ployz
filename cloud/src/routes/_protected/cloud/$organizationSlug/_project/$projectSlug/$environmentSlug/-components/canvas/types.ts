import type { Node } from "@xyflow/react";

export type CanvasResourceType = "service" | "variable_group" | "volume";

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

export type CanvasVariableGroupNodeData = {
  resourceType: "variable_group";
  resourceId: string;
  environmentId: string;
};

export type CanvasVariableGroupNode = Node<CanvasVariableGroupNodeData, "variable_group">;

export type CanvasVolumeNodeData = {
  resourceType: "volume";
  resourceId: string;
  environmentId: string;
};

export type CanvasVolumeNode = Node<CanvasVolumeNodeData, "volume">;

export type CanvasResourceNode =
  | CanvasServiceNode
  | CanvasVariableGroupNode
  | CanvasVolumeNode;

export type FlowPosition = { x: number; y: number };
export type CreatorPanel = "root" | "git" | "image";
