import { StoreServiceCard } from "./StoreServiceNode";
import { StoreVolumeCard } from "./StoreVolumeNode";
import { StoreConfigCard } from "./StoreConfigNode";
import { ServiceTrays } from "./VolumeTray";
import type { StoreCanvas } from "./types";
import { CopyHang } from "./MirrorNode";
import { hangByNode } from "./nodes";

export function CanvasNodeList({
  store,
  selectedNodeId,
}: {
  store: Pick<StoreCanvas, "services" | "unmountedVolumes" | "unmountedConfigs"> & Partial<Pick<StoreCanvas, "copies">>;
  selectedNodeId: string | null;
}) {
  const { services, unmountedVolumes, unmountedConfigs } = store;
  const hang = hangByNode(store);
  return (
    <div
      className="canvas-node-list absolute inset-0 overflow-y-auto px-4 pb-4 pt-16 min-[861px]:hidden"
    >
      <div className="flex flex-col gap-3">
        {services.map((service) => (
          <div key={service.service.id}>
            <StoreServiceCard {...service} selected={service.service.id === selectedNodeId} compact className="block" />
            <ServiceTrays trays={service.trays} configTrays={service.configTrays} selectedNodeId={selectedNodeId} />
            <CopyHang hang={hang.get(service.service.id) ?? []} />
          </div>
        ))}
        {unmountedVolumes.map((volume) => (
          <div key={volume.id}>
            <StoreVolumeCard volume={volume} selected={volume.id === selectedNodeId} className="block" />
            <CopyHang hang={hang.get(volume.id) ?? []} />
          </div>
        ))}
        {unmountedConfigs.map((config) => (
          <StoreConfigCard key={config.id} config={config} selected={config.id === selectedNodeId} className="block" />
        ))}
      </div>
    </div>
  );
}
