import { StoreServiceCard } from "./StoreServiceNode";
import { StoreVolumeCard } from "./StoreVolumeNode";
import { StoreConfigCard } from "./StoreConfigNode";
import { StoreLiveCard } from "./StoreLiveNode";
import { StoreTemplateCard } from "./StoreTemplateNode";
import { ServiceTrays } from "./VolumeTray";
import type { CanvasResourceNode } from "./types";

/** The canvas's own nodes, one under another, for screens too narrow for the canvas: a template stays one item here too. */
export function CanvasNodeList({ nodes, selectedNodeId }: { nodes: readonly CanvasResourceNode[]; selectedNodeId: string | null }) {
  return (
    <div
      className="canvas-node-list absolute inset-0 overflow-y-auto px-4 pb-4 pt-16 min-[861px]:hidden"
    >
      <div className="flex flex-col gap-3">
        {nodes.map((node) => <div key={node.id}>{listItem(node, selectedNodeId)}</div>)}
      </div>
    </div>
  );
}

function listItem(node: CanvasResourceNode, selectedNodeId: string | null) {
  switch (node.type) {
    case "storeService":
      return <>
        <StoreServiceCard {...node.data} selected={node.id === selectedNodeId} compact className="block" />
        <ServiceTrays trays={node.data.trays} configTrays={node.data.configTrays} selectedNodeId={selectedNodeId} />
      </>;
    case "storeVolume":
      return <StoreVolumeCard volume={node.data.volume} selected={node.id === selectedNodeId} className="block" />;
    case "storeConfig":
      return <StoreConfigCard config={node.data.config} selected={node.id === selectedNodeId} className="block" />;
    case "storeLive":
      return <StoreLiveCard live={node.data.live} className="block h-24" />;
    case "storeTemplate":
      return <StoreTemplateCard instance={node.data.instance} className="mb-3 h-36 w-full" />;
    default:
      return node satisfies never;
  }
}
