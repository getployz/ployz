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
import { ApplyZone } from "./ApplyZone";
import { SNAP_GRID } from "./constants";
import { canvasNodeTypes } from "./canvas-node-types";
import { CanvasNodeList } from "./CanvasServiceList";
import { CanvasServicesProvider } from "./CanvasServicesContext";
import { useCanvasPositionMutation } from "./useCanvasPositionMutation";
import { blurClickedNodeLink, useCanvasNavigation } from "./useCanvasNavigation";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { useServiceCreator } from "./useServiceCreator";
import { useVolumeCreator } from "./useVolumeCreator";
import { CanvasContextMenu } from "./CanvasContextMenu";
import { ServiceCreatorDialog } from "./ServiceCreatorDialog";
import { VolumeCreatorDialog } from "./VolumeCreatorDialog";
import { useCanvasChangeActions } from "./useCanvasChangeActions";
import { useCanvasFlowState } from "./useCanvasFlowState";
import {
  ENVIRONMENT_ROUTE_FROM,
  ENVIRONMENT_SERVICE_ROUTE_TO,
} from "../environment-route-paths";
import type { CanvasResourceNode } from "./types";
import { DestructiveConfirmationDialog } from "#/components/destructive-volume/volume-destruction-confirmation-dialog";
import type { EnvironmentNodeIntroduction } from "#/modules/environment-design/environment-node-introductions";

// Shared styling for every canvas edge: dashed, primary colour, matching arrow.
const DEFAULT_EDGE_OPTIONS = {
  type: "smoothstep",
  style: { stroke: "var(--primary)", strokeDasharray: "6 4" },
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
  const navigate = useNavigate();
  const locationKey = useLocation({ select: (location) => location.href });
  const { onNodeDrag } = useCanvasPositionMutation({
    ...params,
    organizationId,
  });
  const { selectedNodeId } = useCanvasInspectorSelection();
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
  });
  const { getViewportCenter } = useCanvasNavigation(
    selectedNodeId,
    selectedNodePositionKey,
    flowReady,
  );
  const creator = useServiceCreator(params, environmentId, getViewportCenter);
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
              edges={canvasEdges}
              defaultEdgeOptions={DEFAULT_EDGE_OPTIONS}
              nodeTypes={canvasNodeTypes}
              elementsSelectable={false}
              nodesFocusable={false}
              fitView={!selectedNodeId}
              proOptions={{ hideAttribution: true }}
              snapToGrid
              snapGrid={SNAP_GRID}
              minZoom={0.4}
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
        services={activeServicesWithBoundEnv}
        selectedNodeId={selectedNodeId}
        servicesById={servicesById}
        volumeResourcesById={volumeResourcesById}
      />
      <div className="pointer-events-none absolute top-4 right-4 flex items-center gap-2">
        <Button
          className="pointer-events-auto"
          onClick={() => creator.openCreatorAtCenter()}
        >
          <PlusIcon data-icon="inline-start" />
          Create
        </Button>
      </div>
      </div>

      <ApplyZone
          key={locationKey}
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
      <DestructiveConfirmationDialog
        open={destructiveConfirmationOpen}
        onOpenChange={setDestructiveConfirmationOpen}
        confirmPhrase={params.environmentSlug}
        serviceNames={destructiveServiceNames}
        title={reviewAction === "deploy" ? "Deploy destructive changes?" : "Save destructive changes?"}
        description={reviewAction === "deploy" ? "Review every Service and Volume removal. Confirmation publishes this revision and deploys it, including the reviewed removals." : "Review every Service and Volume removal. Confirmation saves this revision without changing running resources."}
        actionLabel={reviewAction === "deploy" ? "Deploy removals" : "Save removals"}
        pendingActionLabel={reviewAction === "deploy" ? "Deploying..." : "Saving..."}
        callbacks={{
          load: prepareDestructiveReview,
          confirm: confirmDestructiveAction,
        }}
      />
    </>
  );
}
