import type { DiffView, DomainRow, ServiceListing, VolumeListing } from "@ployz/sdk";
import type { Node } from "@xyflow/react";

export type CanvasResourceType = "service" | "volume";

/** A Service as the Config Store lists it, with what its card shows. */
export type StoreCanvasService = {
  service: ServiceListing;
  /** Its public domains, with their status. */
  domains: DomainRow[];
  /** How many of its Settings the next Deploy changes. */
  changeCount: number;
  /** How runtime evidence names it, `NAMESPACE/PRIVATE_DNS`; null when the Environment has no Namespace. */
  runtimeIdentity: string | null;
  /** An empty Service here last deployed from an Uploaded Source (`deploy --upload`). */
  uploaded: boolean;
  /** How many replicas it asks for, when its Settings say. */
  desiredReplicas: number | null;
};

/** A Volume as a tray under a Service that mounts it; `sharedWith`: the other Services here that mount it. */
export type VolumeTray = { volume: VolumeListing; sharedWith: string[] };

export type CanvasStoreServiceNode = Node<StoreCanvasService & {
  trays: VolumeTray[];
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

export type CanvasResourceNode =
  | CanvasStoreServiceNode
  | CanvasStoreVolumeNode
  | CanvasStoreLiveNode;

export type FlowPosition = { x: number; y: number };
