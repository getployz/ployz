import type { VolumeListing } from "@ployz/sdk";
import { StoreServiceCard } from "./StoreServiceNode";
import { StoreVolumeCard } from "./StoreVolumeNode";
import type { StoreCanvasService } from "./types";

/** The canvas as a list, on phones: its Services, then its Volumes. */
export function CanvasNodeList({
  services,
  volumes,
  selectedNodeId,
}: {
  services: StoreCanvasService[];
  volumes: VolumeListing[];
  selectedNodeId: string | null;
}) {
  return (
    <div
      className="canvas-node-list absolute inset-0 overflow-y-auto px-4 pb-4 pt-16 min-[861px]:hidden"
    >
      <div className="flex flex-col gap-3">
        {services.map((service) => (
          <StoreServiceCard key={service.service.id} {...service} selected={service.service.id === selectedNodeId} className="block" />
        ))}
        {volumes.map((volume) => (
          <StoreVolumeCard key={volume.id} volume={volume} selected={volume.id === selectedNodeId} className="block" />
        ))}
      </div>
    </div>
  );
}
