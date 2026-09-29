import type { DiffView, ServiceListing, VolumeListing } from "@ployz/sdk";
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

/** A Service as the Config Store lists it, with what its card shows. */
export type StoreCanvasService = {
  service: ServiceListing;
  /** Its image or repository. */
  subtitle: string | null;
  /** How many of its Settings the next Deploy changes. */
  changeCount: number;
};

export type CanvasStoreServiceNode = Node<StoreCanvasService & {
  resourceType: "service";
  resourceId: string;
  environmentId: string;
}, "storeService">;

/** A Config Store Volume on the canvas, as the Store lists it. */
export type CanvasStoreVolumeNode = Node<{
  volume: VolumeListing;
  resourceType: "volume";
  resourceId: string;
  environmentId: string;
}, "storeVolume">;

/** A node a Branch uses live over the Config Store, by its name where it runs. */
export type StoreLiveNode = {
  name: string;
  /** The Environment it runs in; null when none does. */
  owner: string | null;
  /** It holds its owner's real data. */
  data: boolean;
  /** The Services here that read it, by id. */
  usedBy: string[];
};

export type CanvasStoreLiveNode = Node<{ live: StoreLiveNode }, "storeLive">;

/** What the canvas draws from the Config Store while it backs this Environment. */
export type StoreCanvas = {
  services: StoreCanvasService[];
  volumes: VolumeListing[];
  /** A Branch's Live Nodes; none elsewhere. */
  live: StoreLiveNode[];
  /** What the next Deploy changes: the bottom bar's count and its Details. */
  diff: DiffView;
};

/** A Branch's Live Node: another Environment's service its Own Copies use live. */
export type CanvasLiveNode = Node<{ liveNode: LiveNode }, "live">;

export type CanvasResourceNode =
  | CanvasServiceNode
  | CanvasStoreServiceNode
  | CanvasVolumeNode
  | CanvasStoreVolumeNode
  | CanvasStoreLiveNode
  | CanvasLiveNode;

export type FlowPosition = { x: number; y: number };
export type CreatorPanel = "root" | "git" | "image";
