import { useState } from "react";
import {
  Background,
  BackgroundVariant,
  type Edge,
  MarkerType,
  ReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useLocation, useNavigate, useParams } from "@tanstack/react-router";
import { PlusIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import type { VolumeResourceRecord } from "#/modules/environment-design/resources";
import type { EnvironmentChangeStateProjection } from "#/modules/deployments/deployment-contract";
import type { EnvironmentServiceViewRecord } from "#/modules/services/services.collection";
import { BottomBar } from "./BottomBar";
import { CANVAS_MIN_ZOOM, SNAP_GRID } from "./constants";
import { canvasNodeTypes } from "./canvas-node-types";
import { CanvasNodeList } from "./CanvasServiceList";
import { CanvasServicesProvider } from "./CanvasServicesContext";
import { useCanvasPositionMutation } from "./useCanvasPositionMutation";
import { blurClickedNodeLink, useCanvasNavigation } from "./useCanvasNavigation";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { useDeploymentFocus } from "../deployment-page";
import { useServiceCreator } from "./useServiceCreator";
import { LIVE_EDGE_STYLE } from "./nodes";
import { useBranchPicking } from "../new-branch/branch-picking";
import { useVolumeCreator } from "./useVolumeCreator";
import { CanvasContextMenu } from "./CanvasContextMenu";
import { CanvasFinder } from "./CanvasFinder";
import { IdleCloseWarning } from "./IdleCloseWarning";
import { useEnvironmentNavigationNodes } from "../environment-node-navigation";
import { ServiceCreatorDialog } from "./ServiceCreatorDialog";
import { VolumeCreatorDialog } from "./VolumeCreatorDialog";
import { useCanvasChangeActions } from "./useCanvasChangeActions";
import { useCanvasFlowState } from "./useCanvasFlowState";
import { useEnvironmentDeployments } from "#/modules/deployments/deployment.collection";
import { useShutdown } from "#/modules/branches/branch.collection";
import {
  ENVIRONMENT_ROUTE_FROM,
  ENVIRONMENT_SERVICE_ROUTE_TO,
} from "../environment-route-paths";
import type { CanvasResourceNode } from "./types";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { DestructiveChangesDialog } from "./DestructiveChangesDialog";
import { getServiceIcon } from "./service-node-helpers";
import type { EnvironmentNodeIntroduction } from "#/modules/environment-design/environment-node-introductions";

// Shared styling for every canvas edge: solid, primary colour, matching arrow. Links into Live Nodes are dashed.
const DEFAULT_EDGE_OPTIONS = {
  type: "smoothstep",
  style: { stroke: "var(--primary)" },
  markerEnd: { type: MarkerType.ArrowClosed, color: "var(--primary)" },
} as const;

export function CanvasFlow({
  organizationId,
  environmentId,
  servicesWithBoundEnv,
  volumeResources,
  environmentChangeState,
  nodeIntroductions,
  canvasNodes,
  canvasEdges,
}: {
  organizationId: string;
  environmentId: string;
  servicesWithBoundEnv: EnvironmentServiceViewRecord[];
  volumeResources: VolumeResourceRecord[];
  environmentChangeState: EnvironmentChangeStateProjection | null;
  nodeIntroductions: EnvironmentNodeIntroduction[];
  canvasNodes: CanvasResourceNode[];
  canvasEdges: Edge[];
}) {
  const activeServicesWithBoundEnv = servicesWithBoundEnv.filter(
    (service) => service.service.deletedAt == null,
  );
  const [flowReady, setFlowReady] = useState(false);
  const [commitMessage, setCommitMessage] = useState("");
  const [destructiveConfirmationOpen, setDestructiveConfirmationOpen] =
    useState(false);
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const branch = useWorkspace(params.organizationSlug).branches.find((row) => row.environmentId === environmentId);
  const navigate = useNavigate();
  const locationKey = useLocation({ select: (location) => location.href });
  const { onNodeDrag } = useCanvasPositionMutation({
    ...params,
    organizationId,
  });
  const { selectedNodeId } = useCanvasInspectorSelection();
  const findableNodes = useEnvironmentNavigationNodes(params).nodes;
  const {
    canvasChangeState,
    diffGroups,
    totalChanges,
    canDeploy,
    canSave,
    servicesById,
    selectedNodePositionKey,
    volumeResourcesById,
    destructiveServiceIds,
    destructiveServiceNames,
    deletedDeployedVolumeIds,
  } = useCanvasFlowState({
    servicesWithBoundEnv,
    volumeResources,
    environmentChangeState,
    nodeIntroductions,
    canvasNodes,
    selectedNodeId,
    // The latest attempt's: admission records them afresh, so a deploy that resolves them clears the amber.
    missingLiveValues: useEnvironmentDeployments(params.organizationSlug, environmentId)[0]?.deployment.missingLiveValues ?? [],
    off: useShutdown(params.organizationSlug, environmentId) === "off",
  });
  // While picking a Branch, links into what it would use live are dashed.
  const picking = useBranchPicking();
  const liveRoles = new Set(picking?.plan.nodes.flatMap((node) => node.role === "live" ? [node.lineageId] : []));
  const pickedLiveIds = new Set([
    ...[...servicesById].flatMap(([id, state]) => liveRoles.has(state.serviceView.service.lineageId) ? [id] : []),
    ...[...volumeResourcesById].flatMap(([id, state]) => liveRoles.has(state.resource.resource.lineageId) ? [id] : []),
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
  const idleCloseWarning = { organizationSlug: params.organizationSlug, environmentId };
  const volumeCreator = useVolumeCreator(
    params,
    environmentId,
    getViewportCenter,
  );
  const {
    discardAllChanges,
    discardNodeChanges,
    discardRowChange,
    requestSave,
    requestDeploy,
    isSubmittingDeploymentSnapshot,
    prepareDestructiveReview,
    confirmDestructiveAction,
    reviewAction,
  } = useCanvasChangeActions({
    environmentId,
    params,
    changeState: canvasChangeState,
    savedSnapshotSource: environmentChangeState?.saved
      ? {
          kind: "saved",
          environmentSavedStateSnapshotId:
            environmentChangeState.saved.snapshotId,
        }
      : null,
    destructiveServiceIds,
    deletedDeployedVolumeIds,
    // A Branch that isn't kept deletes with no typed confirmation: its Own Copies started empty.
    confirmsRemovals: branch === undefined || branch.kept,
    commitMessage,
    setCommitMessage,
    setDestructiveConfirmationOpen,
  });

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
          <CanvasServicesProvider
            servicesById={servicesById}
            volumeResourcesById={volumeResourcesById}
          >
            <ReactFlow
              key={`${params.projectSlug}/${params.environmentSlug}`}
              nodes={canvasNodes}
              edges={edges}
              defaultEdgeOptions={DEFAULT_EDGE_OPTIONS}
              nodeTypes={canvasNodeTypes}
              elementsSelectable={false}
              nodesFocusable={false}
              fitView={!selectedNodeId}
              proOptions={{ hideAttribution: true }}
              snapToGrid
              snapGrid={SNAP_GRID}
              minZoom={CANVAS_MIN_ZOOM}
              maxZoom={1.35}
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
          </CanvasServicesProvider>
        </CanvasContextMenu>
      </div>
      <CanvasNodeList
        header={<IdleCloseWarning {...idleCloseWarning} />}
        services={activeServicesWithBoundEnv}
        liveNodes={canvasNodes.flatMap((node) => node.type === "live" ? [node.data.liveNode] : [])}
        selectedNodeId={selectedNodeId}
        servicesById={servicesById}
        volumeResourcesById={volumeResourcesById}
      />
      {/* Phones show it atop the node list instead. */}
      <IdleCloseWarning {...idleCloseWarning} className="absolute top-4 left-4 max-[860px]:hidden" />
      <div className="pointer-events-none absolute top-4 right-4 flex items-center gap-2">
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

      <BottomBar
          key={locationKey}
          environmentId={environmentId}
          groups={diffGroups}
          totalChanges={totalChanges}
          canDeploy={canDeploy && !isSubmittingDeploymentSnapshot}
          commitMessage={commitMessage}
          canSaveWithoutDeploying={canSave}
          onCommitMessageChange={setCommitMessage}
          onDeploy={() => {
            requestDeploy();
          }}
          onSaveWithoutDeploying={() => {
            requestSave();
          }}
          onDiscardAll={discardAllChanges}
          onDiscardNode={(group) => {
            void discardNodeChanges(group);
          }}
          onDiscardRow={(group, path) => {
            void discardRowChange(group, path);
          }}
        />

      <ServiceCreatorDialog
        open={creator.creatorOpen}
        onOpenChange={creator.setCreatorOpen}
        panel={creator.creatorPanel}
        position={creator.creatorPosition}
        params={params}
        onCreateVolume={openVolumeCreatorFromServiceDialog}
        onCreated={async (result) => {
          creator.setCreatorOpen(false);
          await navigate({
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
        open={volumeCreator.creatorOpen}
        onOpenChange={volumeCreator.setCreatorOpen}
        position={volumeCreator.creatorPosition}
        onCreate={async (input) => {
          await volumeCreator.createVolume(input);
        }}
      />
      <DestructiveChangesDialog
        open={destructiveConfirmationOpen}
        onOpenChange={setDestructiveConfirmationOpen}
        organizationSlug={params.organizationSlug}
        environmentId={environmentId}
        action={reviewAction}
        services={destructiveServiceIds.map((id, index) => {
          const service = servicesWithBoundEnv.find((row) => row.service.id === id)?.service;
          return { kind: "service" as const, name: destructiveServiceNames[index] ?? id, icon: service && getServiceIcon(service) };
        })}
        volumes={deletedDeployedVolumeIds.map((id) => volumeResourcesById.get(id)?.resource.resource.name ?? id)}
        prepare={prepareDestructiveReview}
        confirm={confirmDestructiveAction}
      />
    </>
  );
}
