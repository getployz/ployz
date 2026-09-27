import type { LiveNode } from "#/modules/branches/use-live-nodes";
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

/** A Branch's Live Node: another Environment's service its Own Copies use live. */
export type CanvasLiveNode = Node<{ liveNode: LiveNode }, "live">;

export type CanvasResourceNode =
  | CanvasServiceNode
  | CanvasVolumeNode
  | CanvasLiveNode;

export type FlowPosition = { x: number; y: number };
export type CreatorPanel = "root" | "git" | "image";
