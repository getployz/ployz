import { StoreServiceCard } from "./StoreServiceNode";
import { StoreVolumeCard } from "./StoreVolumeNode";
import { StoreConfigCard } from "./StoreConfigNode";
import { ServiceTrays } from "./VolumeTray";
import type { StoreCanvas } from "./types";

export function CanvasNodeList({
  store: { services, unmountedVolumes, unmountedConfigs },
  selectedNodeId,
}: {
  store: Pick<StoreCanvas, "services" | "unmountedVolumes" | "unmountedConfigs">;
  selectedNodeId: string | null;
}) {
  return (
    <div
      className="canvas-node-list absolute inset-0 overflow-y-auto px-4 pb-4 pt-16 min-[861px]:hidden"
    >
      <div className="flex flex-col gap-3">
        {services.map((service) => (
          <div key={service.service.id}>
            <StoreServiceCard {...service} selected={service.service.id === selectedNodeId} compact className="block" />
            <ServiceTrays trays={service.trays} configTrays={service.configTrays} selectedNodeId={selectedNodeId} />
          </div>
        ))}
        {unmountedVolumes.map((volume) => (
          <StoreVolumeCard key={volume.id} volume={volume} selected={volume.id === selectedNodeId} className="block" />
        ))}
        {unmountedConfigs.map((config) => (
          <StoreConfigCard key={config.id} config={config} selected={config.id === selectedNodeId} className="block" />
        ))}
      </div>
    </div>
  );
}
