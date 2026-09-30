import { useState } from "react";
import {
  Background,
  BackgroundVariant,
  type Edge,
  MarkerType,
  ReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useLoaderData, useLocation, useNavigate, useParams } from "@tanstack/react-router";
import { PlusIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { BottomBar } from "./BottomBar";
import { storeHintNotes } from "../branch-review/store-hints";
import { CANVAS_FIT_VIEW, CANVAS_MAX_ZOOM, CANVAS_MIN_ZOOM, SNAP_GRID } from "./constants";
import { canvasNodeTypes } from "./canvas-node-types";
import { CanvasNodeList } from "./CanvasServiceList";
import { useCanvasPositionMutation } from "./useCanvasPositionMutation";
import { blurClickedNodeLink, useCanvasNavigation } from "./useCanvasNavigation";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { useDeploymentFocus } from "../deployment-page";
import { useServiceCreator } from "./useServiceCreator";
import { LIVE_EDGE_STYLE } from "./nodes";
import { usePickingView } from "../new-branch/branch-picking";
import { useVolumeCreator } from "./useVolumeCreator";
import { useStoreChangeActions } from "./useStoreChangeActions";
import { useSavesInto, useStoreDeployments } from "#/modules/config-store/store-view.queries";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { changeGroups, isInFlight } from "#/modules/config-store/store-deployments";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../deployment-page";
import { CanvasContextMenu } from "./CanvasContextMenu";
import { CanvasFinder } from "./CanvasFinder";
import { BranchButton } from "./BranchButton";
import { ServiceCreatorDialog } from "./ServiceCreatorDialog";
import { VolumeCreatorDialog } from "./VolumeCreatorDialog";
import {
  ENVIRONMENT_ROUTE_FROM,
  ENVIRONMENT_SERVICE_ROUTE_TO,
} from "../environment-route-paths";
import type { CanvasResourceNode, StoreCanvas } from "./types";

// Shared styling for every canvas edge: solid, primary colour, matching arrow. Links into Live Nodes are dashed.
const DEFAULT_EDGE_OPTIONS = {
  type: "smoothstep",
  style: { stroke: "var(--primary)" },
  markerEnd: { type: MarkerType.ArrowClosed, color: "var(--primary)" },
} as const;

export function CanvasFlow({
  organizationId,
  environmentId,
  canvasNodes,
  canvasEdges,
  store,
}: {
  organizationId: string;
  environmentId: string;
  canvasNodes: CanvasResourceNode[];
  canvasEdges: Edge[];
  store: StoreCanvas;
}) {
  const [flowReady, setFlowReady] = useState(false);
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
  const locationKey = useLocation({ select: (location) => location.href });
  const { onNodeDrag } = useCanvasPositionMutation({
    ...params,
    organizationId,
  });
  const { selectedNodeId } = useCanvasInspectorSelection();
  const findableNodes = [...store.services.map(({ service }) => ({ id: service.id, name: service.name, type: "service" as const })),
    ...store.volumes.map((volume) => ({ id: volume.id, name: volume.name, type: "volume" as const }))];
  const selectedNode = canvasNodes.find((node) => node.id === selectedNodeId);
  const selectedNodePositionKey = selectedNode ? `${selectedNode.position.x}:${selectedNode.position.y}` : null;
  // While picking a Branch, links into what it would use live are dashed. A plan names nodes.
  const picking = usePickingView();
  const liveRoles = new Set(picking?.plan.nodes.flatMap((node) => node.role === "live" ? [node.name] : []));
  const pickedLiveIds = new Set([
    ...store.services.flatMap(({ service }) => liveRoles.has(service.name) ? [service.id] : []),
    ...store.volumes.flatMap((volume) => liveRoles.has(volume.name) ? [volume.id] : []),
  ]);
  const edges = pickedLiveIds.size === 0 ? canvasEdges : canvasEdges.map((edge) =>
    pickedLiveIds.has(edge.source) || pickedLiveIds.has(edge.target) ? { ...edge, style: LIVE_EDGE_STYLE } : edge);
  const { getViewportCenter } = useCanvasNavigation(
    selectedNodeId,
    selectedNodePositionKey,
    flowReady,
    // Picking a Branch brings the whole canvas into view beside the panel.
    useDeploymentFocus() ?? (picking ? { key: "new-branch", nodeIds: canvasNodes.map((node) => node.id) } : null),
  );
  const creator = useServiceCreator(params, environmentId, getViewportCenter);
  const volumeCreator = useVolumeCreator(
    params,
    environmentId,
    getViewportCenter,
  );

  function openVolumeCreatorFromServiceDialog() {
    creator.setCreatorOpen(false);
    volumeCreator.openCreatorAtPosition(creator.creatorPosition);
  }

  return (
    <>
      <div className="canvas-graph" inert={selectedNodeId !== null}>
      <div className="hidden h-full min-[861px]:block">
        <CanvasContextMenu
          onCreateFromPanel={creator.openCreatorAtLastRightClick}
          onCreateBlank={creator.createBlankServiceAtLastRightClick}
          onCreateVolume={volumeCreator.openCreatorAtLastRightClick}
        >
          <ReactFlow
            key={`${params.projectSlug}/${params.environmentSlug}`}
            nodes={canvasNodes}
            edges={edges}
            defaultEdgeOptions={DEFAULT_EDGE_OPTIONS}
            nodeTypes={canvasNodeTypes}
            elementsSelectable={false}
            nodesFocusable={false}
            fitView={!selectedNode}
            fitViewOptions={CANVAS_FIT_VIEW}
            proOptions={{ hideAttribution: true }}
            snapToGrid
            snapGrid={SNAP_GRID}
            minZoom={CANVAS_MIN_ZOOM}
            maxZoom={CANVAS_MAX_ZOOM}
            onInit={() => setFlowReady(true)}
            onNodeClick={blurClickedNodeLink}
            onNodeDrag={onNodeDrag}
            onNodeDragStop={onNodeDrag}
            onPaneContextMenu={(event) => {
              creator.onPaneContextMenu(event);
              volumeCreator.onPaneContextMenu(event);
            }}
          >
            <Background variant={BackgroundVariant.Dots} gap={16} size={1} />
          </ReactFlow>
        </CanvasContextMenu>
      </div>
      <CanvasNodeList services={store.services} volumes={store.volumes} selectedNodeId={selectedNodeId} />
      <div className="pointer-events-none absolute top-4 right-4 flex items-center gap-2">
        <BranchButton />
        <CanvasFinder nodes={findableNodes} />
        <Button
          className="pointer-events-auto"
          onClick={() => creator.openCreatorAtCenter()}
        >
          <PlusIcon data-icon="inline-start" />
          Create
        </Button>
      </div>
      </div>

      <StoreBottomBar key={locationKey} store={store} />

      <ServiceCreatorDialog
        open={creator.creatorOpen}
        onOpenChange={creator.setCreatorOpen}
        panel={creator.creatorPanel}
        position={creator.creatorPosition}
        params={params}
        onCreateVolume={openVolumeCreatorFromServiceDialog}
        onCreated={async (result, stillHere) => {
          creator.setCreatorOpen(false);
          if (stillHere) await navigate({
            to: ENVIRONMENT_SERVICE_ROUTE_TO,
            params: {
              organizationSlug: params.organizationSlug,
              projectSlug: params.projectSlug,
              environmentSlug: params.environmentSlug,
              serviceId: result.service.id,
            },
            search: (prev) => prev,
          });
        }}
      />
      <VolumeCreatorDialog
        organizationSlug={params.organizationSlug}
        open={volumeCreator.creatorOpen}
        onOpenChange={volumeCreator.setCreatorOpen}
        position={volumeCreator.creatorPosition}
        onCreate={volumeCreator.createVolume}
      />
    </>
  );
}

/**
 * The bottom bar over the Config Store: the Store's review in Details, Deploy, Save without deploying and Discard
 * through its write queue, and the Environment's in-flight Deployment, whether the CLI or this tab admitted it.
 */
function StoreBottomBar({ store }: { store: StoreCanvas }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
  const ref = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM }).store;
  const { diff } = store;
  const actions = useStoreChangeActions(params.organizationSlug, ref, diff.version,
    (deploymentId) => void navigate({ to: DEPLOYMENT_PAGE_ROUTE_TO, params: { ...params, deploymentId } }));
  const deployments = useStoreDeployments(params.organizationSlug, ref).data.pages[0]?.deployments ?? [];
  const waiting = useSavesInto(params.organizationSlug, params.projectSlug, diff.environment.name);
  const { noServers } = useRuntimeLens(params.organizationSlug);
  const groups = changeGroups(diff, store.services.map(({ service }) => service));
  return (
    <>
      <BottomBar
        groups={groups}
        totalChanges={diff.total_count}
        canPublish={diff.total_count > 0 && !diff.published}
        onDeploy={actions.deploy}
        admitting={actions.admitting}
        onPublish={actions.publish}
        onDiscardAll={() => actions.discard(null)}
        onDiscardNode={(group) => actions.discard(group.discardPath)}
        onDiscardRow={(_, path) => actions.discard(path)}
        active={deployments.filter((deployment) => isInFlight(deployment.status))}
        notes={storeHintNotes(diff, groups)}
        waiting={waiting}
        noServers={noServers}
      />
      {actions.dialog}
    </>
  );
}
