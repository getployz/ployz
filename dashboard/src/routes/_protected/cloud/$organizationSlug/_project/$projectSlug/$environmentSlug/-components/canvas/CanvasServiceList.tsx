import type { VolumeListing } from "@ployz/sdk";
import { ServiceTrays, StoreServiceCard } from "./StoreServiceNode";
import { StoreVolumeCard } from "./StoreVolumeNode";
import { volumeTrays } from "./nodes";
import type { StoreCanvasService } from "./types";

/** The canvas as a list, on phones: its Services as compact cards with their Volume trays, then the Volumes nothing mounts. */
export function CanvasNodeList({
  services,
  volumes,
  selectedNodeId,
}: {
  services: StoreCanvasService[];
  volumes: VolumeListing[];
  selectedNodeId: string | null;
}) {
  const { trays, unmounted } = volumeTrays(services, volumes);
  return (
    <div
      className="canvas-node-list absolute inset-0 overflow-y-auto px-4 pb-4 pt-16 min-[861px]:hidden"
    >
      <div className="flex flex-col gap-3">
        {services.map((service) => (
          <div key={service.service.id}>
            <StoreServiceCard {...service} selected={service.service.id === selectedNodeId} compact className="block" />
            <ServiceTrays trays={trays.get(service.service.id) ?? []} selectedNodeId={selectedNodeId} />
          </div>
        ))}
        {unmounted.map((volume) => (
          <StoreVolumeCard key={volume.id} volume={volume} selected={volume.id === selectedNodeId} className="block" />
        ))}
      </div>
    </div>
  );
}
