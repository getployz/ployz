import type { ConfigListing, DiffView, DomainRow, ServiceListing, VolumeListing } from "@ployz/sdk";
import type { MountedConfig } from "#/modules/config-store/store-configs";
import type { Node } from "@xyflow/react";

export type CanvasResourceType = "service" | "volume" | "config";

/** A Service as the Config Store lists it, with what its card shows. */
export type StoreCanvasService = {
  service: ServiceListing;
  /** Its public domains, with their status. */
  domains: DomainRow[];
  /** How many of its Settings the next Deploy changes. */
  changeCount: number;
  /** How runtime evidence names it, `NAMESPACE/PRIVATE_DNS`; null when the Environment has no Namespace. */
  runtimeIdentity: string | null;
  /** How many replicas its deployed Settings ask for, when they say. */
  desiredReplicas: number | null;
  /** The Volumes it mounts, as trays under its card. */
  trays: MountedVolume[];
  /** The Configs it mounts, as trays under its card, above its Volumes. */
  configTrays: MountedConfig[];
};

/**
 * A Volume under a Service that mounts it; `sharedWith`: the other Services here that mount it; `mountChanged`: the next
 * Deploy adds or changes this Service's mount of it; `writers`: the containers that write it, every replica of every
 * Service that mounts it.
 */
export type MountedVolume = { volume: VolumeListing; sharedWith: string[]; mountChanged: boolean; writers: number };

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

/** A Config no Service here mounts, on the canvas. */
export type CanvasStoreConfigNode = Node<{
  config: ConfigListing;
  resourceType: "config";
  resourceId: string;
  environmentId: string;
}, "storeConfig">;

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
  configs: ConfigListing[];
  /** The Volumes no Service here mounts, which are nodes of their own. */
  unmountedVolumes: VolumeListing[];
  /** The Configs no Service here mounts, which are nodes of their own. */
  unmountedConfigs: ConfigListing[];
  /** A Branch's Live Nodes; none elsewhere. */
  live: StoreLiveNode[];
  /** What the next Deploy changes: the bottom bar's count and its Details. */
  diff: DiffView;
};

export type CanvasResourceNode =
  | CanvasStoreServiceNode
  | CanvasStoreVolumeNode
  | CanvasStoreConfigNode
  | CanvasStoreLiveNode;

export type FlowPosition = { x: number; y: number };
