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
import { usePickingView } from "../new-branch/branch-picking";
import { useVolumeCreator } from "./useVolumeCreator";
import { useStoreChangeActions } from "./useStoreChangeActions";
import { useStoreDeployments } from "#/modules/config-store/store-view.queries";
import { changeGroups, isInFlight } from "#/modules/config-store/store-deployments";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../deployment-page";
import { CanvasContextMenu } from "./CanvasContextMenu";
import { CanvasFinder } from "./CanvasFinder";
import { BranchButton } from "./BranchButton";
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
import type { CanvasResourceNode, StoreCanvas } from "./types";
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
  store,
}: {
  organizationId: string;
  environmentId: string;
  servicesWithBoundEnv: EnvironmentServiceViewRecord[];
  volumeResources: VolumeResourceRecord[];
  environmentChangeState: EnvironmentChangeStateProjection | null;
  nodeIntroductions: EnvironmentNodeIntroduction[];
  canvasNodes: CanvasResourceNode[];
  canvasEdges: Edge[];
  /** The Config Store's Services and Volumes, which replace the legacy ones on the canvas; null while the Store is dark. */
  store: StoreCanvas | null;
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
  const navigationNodes = useEnvironmentNavigationNodes(params).nodes;
  const findableNodes = store
    ? [...store.services.map(({ service }) => ({ id: service.id, name: service.name, type: "service" as const })),
      ...store.volumes.map((volume) => ({ id: volume.id, name: volume.name, type: "volume" as const }))]
    : navigationNodes;
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
  const picking = usePickingView();
  const liveRoles = new Set(picking?.plan.nodes.flatMap((node) => node.role === "live" ? [node.lineageId] : []));
  const pickedLiveIds = new Set([
    ...[...servicesById].flatMap(([id, state]) => liveRoles.has(state.serviceView.service.lineageId) ? [id] : []),
    ...[...volumeResourcesById].flatMap(([id, state]) => liveRoles.has(state.resource.resource.lineageId) ? [id] : []),
    // Over the Store, a plan names nodes.
    ...(store?.services ?? []).flatMap(({ service }) => liveRoles.has(service.name) ? [service.id] : []),
    ...(store?.volumes ?? []).flatMap((volume) => liveRoles.has(volume.name) ? [volume.id] : []),
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
        services={activeServicesWithBoundEnv}
        storeServices={store?.services ?? null}
        storeVolumes={store?.volumes ?? null}
        liveNodes={canvasNodes.flatMap((node) => node.type === "live" ? [node.data.liveNode] : [])}
        selectedNodeId={selectedNodeId}
        servicesById={servicesById}
        volumeResourcesById={volumeResourcesById}
      />
      <div className="pointer-events-none absolute top-4 right-4 flex items-center gap-2">
        <BranchButton environmentId={environmentId} />
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

      {store ? <StoreBottomBar key={locationKey} environmentId={environmentId} store={store} /> : <BottomBar
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
        />}

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

/**
 * The bottom bar over the Config Store: the Store's review in Details, Deploy, Save without deploying and Discard
 * through its write queue, and the Environment's in-flight Deployment, whether the CLI or this tab admitted it.
 */
function StoreBottomBar({ environmentId, store }: { environmentId: string; store: StoreCanvas }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
  const ref = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM }).store;
  const actions = useStoreChangeActions(params.organizationSlug, ref, environmentId,
    (deploymentId) => void navigate({ to: DEPLOYMENT_PAGE_ROUTE_TO, params: { ...params, deploymentId } }));
  const deployments = useStoreDeployments(params.organizationSlug, ref).data.pages[0]?.deployments ?? [];
  const { diff } = store;
  return (
    <>
      <BottomBar
        environmentId={environmentId}
        groups={changeGroups(diff, store.services.map(({ service }) => service))}
        totalChanges={diff.total_count}
        canDeploy
        commitMessage=""
        canSaveWithoutDeploying={diff.total_count > 0 && !diff.published}
        onDeploy={actions.deploy}
        onSaveWithoutDeploying={actions.publish}
        onDiscardAll={() => actions.discard(null)}
        onDiscardNode={(group) => void actions.discard(group.nodeName)}
        onDiscardRow={(_, path) => void actions.discard(path)}
        storeActive={deployments.filter((deployment) => isInFlight(deployment.status))}
      />
      {actions.dialog}
    </>
  );
}
