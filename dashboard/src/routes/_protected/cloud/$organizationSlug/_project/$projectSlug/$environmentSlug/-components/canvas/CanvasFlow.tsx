import { use, useState } from "react";
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
import { useCanvasNavigation } from "./useCanvasNavigation";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { useDeploymentFocus } from "../deployment-page";
import { useServiceCreator } from "./useServiceCreator";
import { canvasNodeOf, shownNodeIds } from "./nodes";
import { LitVolumeProvider } from "./VolumeTray";
import { usePickingView } from "../new-branch/branch-picking";
import { useVolumeCreator } from "./useVolumeCreator";
import { useStoreChangeActions } from "./useStoreChangeActions";
import { useInFlightDeployments, useSavesInto } from "#/modules/config-store/store-view.queries";
import { changeGroups } from "#/modules/config-store/store-deployments";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../deployment-page";
import { CanvasContextMenu } from "./CanvasContextMenu";
import { CanvasFinder } from "./CanvasFinder";
import { SyncButton } from "../sync/SyncButton";
import { ServiceCreatorDialog } from "./ServiceCreatorDialog";
import { VolumeCreatorDialog } from "./VolumeCreatorDialog";
import { useKeyboardFocusModality } from "../keyboard-focus-modality";
import { RuntimeLensContext } from "./RuntimeLensProvider";
import {
  ENVIRONMENT_ROUTE_FROM,
  ENVIRONMENT_SERVICE_ROUTE_TO,
} from "../environment-route-paths";
import type { CanvasResourceNode, StoreCanvas } from "./types";

// Every canvas edge is a Live Node's link, dashed by `buildStoreEdges`; each is a smooth step ending in an arrow.
const DEFAULT_EDGE_OPTIONS = {
  type: "smoothstep",
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
  // A Volume in a tray shows on its Service's node, so that is the node the canvas brings into view.
  const focusNodeId = selectedNodeId === null ? null : canvasNodeOf(canvasNodes, selectedNodeId) ?? selectedNodeId;
  const focusNode = canvasNodes.find((node) => node.id === focusNodeId);
  const focusPositionKey = focusNode ? `${focusNode.position.x}:${focusNode.position.y}` : null;
  const picking = usePickingView();
  const deploymentFocus = useDeploymentFocus();
  const { getViewportCenter } = useCanvasNavigation(
    focusNodeId,
    focusPositionKey,
    flowReady,
    deploymentFocus
      ? { key: deploymentFocus.key, nodeIds: shownNodeIds(canvasNodes, deploymentFocus.nodeIds) }
      // Picking a Branch brings the whole canvas into view beside the panel.
      : picking ? { key: "new-branch", nodeIds: canvasNodes.map((node) => node.id) } : null,
  );
  useKeyboardFocusModality();
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
    <LitVolumeProvider>
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
            edges={canvasEdges}
            defaultEdgeOptions={DEFAULT_EDGE_OPTIONS}
            nodeTypes={canvasNodeTypes}
            elementsSelectable={false}
            nodesFocusable={false}
            fitView={!focusNode}
            fitViewOptions={CANVAS_FIT_VIEW}
            proOptions={{ hideAttribution: true }}
            snapToGrid
            snapGrid={SNAP_GRID}
            minZoom={CANVAS_MIN_ZOOM}
            maxZoom={CANVAS_MAX_ZOOM}
            onInit={() => setFlowReady(true)}
            // React Flow gives a node pointer events only when it has a click handler (or is selectable), so without
            // one clicks fall through to the pane and the node's link never opens.
            onNodeClick={() => {}}
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
      <CanvasNodeList store={store} selectedNodeId={selectedNodeId} />
      <div className="pointer-events-none absolute top-4 right-4 flex items-center gap-2">
        <SyncButton />
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
    </LitVolumeProvider>
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
  const inFlight = useInFlightDeployments(params.organizationSlug, ref);
  const waiting = useSavesInto(params.organizationSlug, params.projectSlug, diff.environment.name);
  const { noServers } = use(RuntimeLensContext);
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
        active={inFlight}
        notes={storeHintNotes(diff, groups, actions.neverSync, store.parent)}
        waiting={waiting}
        noServers={noServers}
      />
      {actions.dialog}
    </>
  );
}
