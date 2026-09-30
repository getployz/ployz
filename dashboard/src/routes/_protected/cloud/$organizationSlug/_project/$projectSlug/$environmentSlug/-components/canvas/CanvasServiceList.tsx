import { StoreServiceCard } from "./StoreServiceNode";
import { StoreVolumeCard } from "./StoreVolumeNode";
import { ServiceTrays } from "./VolumeTray";
import type { StoreCanvas } from "./types";

/** The canvas as a list, on phones: its Services as compact cards with their Volume trays, then the Volumes nothing mounts. */
export function CanvasNodeList({
  store: { services, unmountedVolumes, runtimeLens },
  selectedNodeId,
}: {
  store: Pick<StoreCanvas, "services" | "unmountedVolumes" | "runtimeLens">;
  selectedNodeId: string | null;
}) {
  return (
    <div
      className="canvas-node-list absolute inset-0 overflow-y-auto px-4 pb-4 pt-16 min-[861px]:hidden"
    >
      <div className="flex flex-col gap-3">
        {services.map((service) => (
          <div key={service.service.id}>
            <StoreServiceCard {...service} runtimeLens={runtimeLens} selected={service.service.id === selectedNodeId} compact className="block" />
            <ServiceTrays trays={service.trays} selectedNodeId={selectedNodeId} />
          </div>
        ))}
        {unmountedVolumes.map((volume) => (
          <StoreVolumeCard key={volume.id} volume={volume} selected={volume.id === selectedNodeId} className="block" />
        ))}
      </div>
    </div>
  );
}
