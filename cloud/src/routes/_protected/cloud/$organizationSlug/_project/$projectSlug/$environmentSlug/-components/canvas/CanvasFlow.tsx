import { useState } from "react";
import {
  Background,
  BackgroundVariant,
  type Edge,
  MarkerType,
  ReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PlusIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import type {
  VariableGroupResourceRecord,
  VolumeResourceRecord,
} from "#/modules/environment-design/resources";
import type { EnvironmentChangeStateProjection } from "#/modules/deployments/deployment-contract";
import type { EnvironmentServiceViewRecord } from "#/modules/services/services.collection";
import {
  useRuntimePublicUrl,
  useRuntimeServices,
  useRuntimeStatus,
} from "#/providers/runtime-provider";
import { ApplyChangesBar } from "./ApplyChangesBar";
import { SNAP_GRID } from "./constants";
import { canvasNodeTypes } from "./canvas-node-types";
import { CanvasServiceList } from "./CanvasServiceList";
import { CanvasServicesProvider } from "./CanvasServicesContext";
import { useCanvasPositionMutation } from "./useCanvasPositionMutation";
import { useCanvasNavigation } from "./useCanvasNavigation";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { useServiceCreator } from "./useServiceCreator";
import { useVariableGroupCreator } from "./useVariableGroupCreator";
import { useVolumeCreator } from "./useVolumeCreator";
import { CanvasContextMenu } from "./CanvasContextMenu";
import { ServiceCreatorDialog } from "./ServiceCreatorDialog";
import { VariableGroupCreatorDialog } from "./VariableGroupCreatorDialog";
import { VolumeCreatorDialog } from "./VolumeCreatorDialog";
import { useCanvasChangeActions } from "./useCanvasChangeActions";
import { useCanvasEscapeShortcut } from "./useCanvasEscapeShortcut";
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
  environmentResources,
  volumeResources,
  environmentChangeState,
  nodeIntroductions,
  canvasNodes,
  canvasEdges,
}: {
  organizationId: string;
  environmentId: string;
  servicesWithBoundEnv: EnvironmentServiceViewRecord[];
  environmentResources: VariableGroupResourceRecord[];
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
  const { status: runtimeStatus } = useRuntimeStatus();
  const { autoDomain } = useRuntimePublicUrl();
  const { runtimeServices, isLoading: runtimeServicesAreLoading } =
    useRuntimeServices(params.environmentSlug);
  const { onNodeDrag } = useCanvasPositionMutation({
    ...params,
    organizationId,
  });
  const { selectedServiceId, selectedNodeId } = useCanvasInspectorSelection();
  const {
    canvasChangeState,
    changeSlices,
    diffGroups,
    totalChanges,
    canDeploy,
    canSave,
    servicesById,
    selectedNodePositionKey,
    environmentResourcesById,
    volumeResourcesById,
    destructiveServiceIds,
    destructiveServiceNames,
    deletedDeployedVolumeIds,
    deployBarPositionClass,
  } = useCanvasFlowState({
    environmentNamespace: params.environmentSlug,
    servicesWithBoundEnv,
    environmentResources,
    volumeResources,
    environmentChangeState,
    nodeIntroductions,
    runtimeServices,
    runtimeStatus,
    runtimeIsLoading: runtimeServicesAreLoading,
    autoDomain,
    canvasNodes,
    selectedNodeId,
  });
  const { selectNode, getViewportCenter } = useCanvasNavigation(
    selectedNodeId,
    selectedNodePositionKey,
    flowReady,
  );
  const creator = useServiceCreator(params, environmentId, getViewportCenter);
  const variableGroupCreator = useVariableGroupCreator(
    params,
    environmentId,
    getViewportCenter,
  );
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
    prepareDestructiveReview,
    confirmDestructiveAction,
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
  useCanvasEscapeShortcut({
    params,
    selectedNodeId,
  });

  function handleNodeClick(node: Parameters<typeof selectNode>[0]) {
    selectNode(node);
  }

  function openVariableGroupCreatorFromServiceDialog() {
    creator.setCreatorOpen(false);
    variableGroupCreator.openCreatorAtPosition(creator.creatorPosition);
  }

  function openVolumeCreatorFromServiceDialog() {
    creator.setCreatorOpen(false);
    volumeCreator.openCreatorAtPosition(creator.creatorPosition);
  }

  return (
    <>
      <div className="hidden h-full sm:block">
        <CanvasContextMenu
          onCreateFromPanel={creator.openCreatorAtLastRightClick}
          onCreateBlank={creator.createBlankServiceAtLastRightClick}
          onCreateVariableGroup={
            variableGroupCreator.openCreatorAtLastRightClick
          }
          onCreateVolume={volumeCreator.openCreatorAtLastRightClick}
        >
          <CanvasServicesProvider
            servicesById={servicesById}
            environmentResourcesById={environmentResourcesById}
            volumeResourcesById={volumeResourcesById}
          >
            <ReactFlow
              key={`${params.projectSlug}/${params.environmentSlug}`}
              nodes={canvasNodes}
              edges={canvasEdges}
              defaultEdgeOptions={DEFAULT_EDGE_OPTIONS}
              nodeTypes={canvasNodeTypes}
              fitView={!selectedNodeId}
              proOptions={{ hideAttribution: true }}
              snapToGrid
              snapGrid={SNAP_GRID}
              minZoom={0.4}
              maxZoom={1.35}
              onInit={() => setFlowReady(true)}
              onNodeClick={(_, node) => {
                handleNodeClick(node);
              }}
              onNodeDrag={onNodeDrag}
              onNodeDragStop={onNodeDrag}
              onPaneContextMenu={(event) => {
                creator.onPaneContextMenu(event);
                variableGroupCreator.onPaneContextMenu(event);
                volumeCreator.onPaneContextMenu(event);
              }}
            >
              <Background variant={BackgroundVariant.Dots} gap={16} size={1} />
            </ReactFlow>
          </CanvasServicesProvider>
        </CanvasContextMenu>
      </div>
      <CanvasServiceList
        services={activeServicesWithBoundEnv}
        selectedServiceId={selectedServiceId}
        servicesById={servicesById}
        hasChanges={totalChanges > 0}
      />

      <div className={`pointer-events-none absolute ${deployBarPositionClass}`}>
        <ApplyChangesBar
          className={selectedNodeId ? "w-full lg:w-auto" : "w-full sm:w-auto"}
          groups={diffGroups}
          slices={changeSlices}
          totalChanges={totalChanges}
          discardAllPlan={canvasChangeState.discardAllPlan}
          canDeploy={canDeploy}
          deploymentEvidence={canvasChangeState.deploymentEvidence}
          commitMessage={commitMessage}
          canSaveWithoutDeploying={canSave}
          onCommitMessageChange={setCommitMessage}
          onDeploy={() => {
            requestDeploy();
          }}
          onSaveWithoutDeploying={() => {
            requestSave();
          }}
          onDiscardAll={() => {
            void discardAllChanges();
          }}
          onDiscardNode={(group) => {
            void discardNodeChanges(group);
          }}
          onDiscardRow={(group, path) => {
            void discardRowChange(group, path);
          }}
        />
      </div>
      <div className="pointer-events-none absolute top-4 right-4 flex items-center gap-2">
        <Button
          className="pointer-events-auto"
          onClick={() => creator.openCreatorAtCenter()}
        >
          <PlusIcon data-icon="inline-start" />
          Create
        </Button>
      </div>

      <ServiceCreatorDialog
        open={creator.creatorOpen}
        onOpenChange={creator.setCreatorOpen}
        panel={creator.creatorPanel}
        position={creator.creatorPosition}
        params={params}
        onCreateVariableGroup={openVariableGroupCreatorFromServiceDialog}
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
      <VariableGroupCreatorDialog
        open={variableGroupCreator.creatorOpen}
        onOpenChange={variableGroupCreator.setCreatorOpen}
        position={variableGroupCreator.creatorPosition}
        onCreate={async (input) => {
          await variableGroupCreator.createVariableGroup(input);
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
        title="Save destructive changes?"
        description="Review every deployed Service and Volume removal. Confirmation publishes this exact Environment revision as Saved intent; runtime deletion happens only during a volume destroy, not Deploy."
        actionLabel="Save removals"
        pendingActionLabel="Saving..."
        callbacks={{
          load: prepareDestructiveReview,
          confirm: confirmDestructiveAction,
        }}
      />
    </>
  );
}
