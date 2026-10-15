import { Suspense, use, useEffect, useRef, useState } from "react";
import {
  Background,
  BackgroundVariant,
  type Edge,
  MarkerType,
  ReactFlow,
  useReactFlow,
  useStore,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { Link, useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import { HistoryIcon, PlusIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { prefersReducedMotion } from "#/lib/motion";
import { BottomBar } from "./BottomBar";
import { storeHintNotes } from "./store-hints";
import { CANVAS_MAX_ZOOM, CANVAS_MIN_ZOOM, SNAP_GRID } from "./constants";
import { canvasNodeTypes } from "./canvas-node-types";
import { CanvasNodeList } from "./CanvasServiceList";
import { useCanvasPositionMutation } from "./useCanvasPositionMutation";
import { fitViewport, useCanvasNavigation } from "./useCanvasNavigation";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { useDeploymentFocus } from "../deployment-page";
import { useServiceCreator } from "./useServiceCreator";
import { canvasNodeOf, shownNodeIds } from "./nodes";
import { LitVolumeProvider } from "./VolumeTray";
import { usePickingView } from "../new-branch/branch-picking";
import { useVolumeCreator } from "./useVolumeCreator";
import { useConfigCreator } from "./useConfigCreator";
import { ConfigCreatorDialog } from "./ConfigCreatorDialog";
import { useStoreChangeActions } from "./useStoreChangeActions";
import { useStoreDeployments } from "#/modules/config-store/store-view.queries";
import { type ChangeGroup, reviewGroups } from "#/modules/config-store/store-deployments";
import { mergeRestoreGroups } from "#/modules/volume-placement/volume-placement";
import { useStagedPlacements, useStorageSteps } from "#/modules/volume-placement/volume-placement.hooks";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../deployment-page";
import { CanvasContextMenu } from "./CanvasContextMenu";
import { CanvasFinder } from "./CanvasFinder";
import { SyncButton } from "../sync/SyncButton";
import { OnMergeButton } from "../sync/OnMerge";
import { SyncDialog } from "../sync/SyncDialog";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { useStoreBusy, useStoreDiscarding } from "#/modules/config-store/store-pending";
import { ServiceCreatorDialog } from "./ServiceCreatorDialog";
import { VolumeCreatorDialog } from "./VolumeCreatorDialog";
import { useKeyboardFocusModality } from "../keyboard-focus-modality";
import { RuntimeLensContext } from "./RuntimeLensProvider";
import {
  ENVIRONMENT_HISTORY_ROUTE_TO,
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
  const { onNodeDrag } = useCanvasPositionMutation({
    ...params,
    organizationId,
  });
  const { selectedNodeId } = useCanvasInspectorSelection();
  const findableNodes = [...store.services.map(({ service }) => ({ id: service.id, name: service.name, type: "service" as const })),
    ...store.volumes.map((volume) => ({ id: volume.id, name: volume.name, type: "volume" as const })),
    ...store.configs.map((config) => ({ id: config.id, name: config.name, type: "config" as const }))];
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
  useRefitOnWidthChange(flowReady && !focusNode);
  const creator = useServiceCreator(params, environmentId, getViewportCenter);
  const volumeCreator = useVolumeCreator(
    params,
    environmentId,
    getViewportCenter,
  );

  const configCreator = useConfigCreator(params, environmentId, getViewportCenter);

  function openConfigCreatorFromServiceDialog() {
    creator.setCreatorOpen(false);
    configCreator.openCreatorAtPosition(creator.creatorPosition);
  }

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
          onCreateConfig={configCreator.openCreatorAtLastRightClick}
        >
          <ReactFlow
            key={`${params.projectSlug}/${params.environmentSlug}`}
            nodes={canvasNodes}
            edges={canvasEdges}
            defaultEdgeOptions={DEFAULT_EDGE_OPTIONS}
            nodeTypes={canvasNodeTypes}
            elementsSelectable={false}
            nodesFocusable={false}
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
              configCreator.onPaneContextMenu(event);
            }}
          >
            <Background variant={BackgroundVariant.Dots} gap={16} size={1} />
          </ReactFlow>
        </CanvasContextMenu>
      </div>
      <CanvasNodeList store={store} selectedNodeId={selectedNodeId} />
      <div className="pointer-events-none absolute top-4 right-4 flex items-center gap-2">
        <Suspense fallback={null}><OnMergeButton /></Suspense>
        <Button nativeButton={false} className="pointer-events-auto" variant="outline" render={<Link to={ENVIRONMENT_HISTORY_ROUTE_TO} params={params} />}><HistoryIcon />History</Button>
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

      <StoreBottomBar store={store} />

      <ServiceCreatorDialog
        open={creator.creatorOpen}
        onOpenChange={creator.setCreatorOpen}
        panel={creator.creatorPanel}
        position={creator.creatorPosition}
        params={params}
        onCreateVolume={openVolumeCreatorFromServiceDialog}
        onCreateConfig={openConfigCreatorFromServiceDialog}
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
      <ConfigCreatorDialog open={configCreator.creatorOpen} onOpenChange={configCreator.setCreatorOpen} onCreate={configCreator.createConfig} />
    </LitVolumeProvider>
  );
}

/**
 * Opening or closing the agent narrows or widens the canvas, and React Flow keeps its viewport. Each time it measures a
 * new width, fit the canvas again, unless a node holds the view. Keyed on the width alone: a quick open and close can
 * land before React Flow measures, so the agent's state says nothing about which width the view was fitted to. Not
 * `fitView`: over controlled nodes it waits for a node change that never comes.
 */
function useRefitOnWidthChange(enabled: boolean) {
  const width = useStore((state) => state.width);
  const height = useStore((state) => state.height);
  const flow = useReactFlow<CanvasResourceNode>();
  const fittedWidth = useRef(width);
  useEffect(() => {
    // Below the desktop breakpoint the canvas is hidden and measures 0; keep the last real width so the way back refits.
    if (!width) return;
    const previous = fittedWidth.current;
    fittedWidth.current = width;
    if (!previous || previous === width || !enabled) return;
    const nodes = flow.getNodes();
    if (!nodes.length) return;
    void flow.setViewport(fitViewport(nodes, width, height), { duration: prefersReducedMotion() ? 0 : 360 });
  }, [width, height, enabled, flow]);
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
  const placements = useStagedPlacements(params.organizationSlug, ref, diff.environment.id, store.volumes);
  const actions = useStoreChangeActions(params.organizationSlug, ref, diff.version,
    (deploymentId) => void navigate({ to: DEPLOYMENT_PAGE_ROUTE_TO, params: { ...params, deploymentId } }), placements.loss);
  const latest = useStoreDeployments(params.organizationSlug, ref).data.pages[0]?.deployments ?? [];
  const { noServers } = use(RuntimeLensContext);
  const storage = useStorageSteps(params.organizationSlug);
  const storeGroups = reviewGroups(diff, store.services.map(({ service }) => service));
  const groups = mergeRestoreGroups(storeGroups, placements.groups);
  const discardNode = (group: ChangeGroup) => group.owner === "placement" ? placements.discard(group.nodeId) : actions.discard(group.discardPath, group.discardTarget);
  const hasSomethingToSave = (diff.draft_count ?? (diff.holds_staged ? 0 : diff.total_count)) > 0 || diff.included.length > 0;
  const writer = useStoreWriter(params.organizationSlug);
  const [including, setIncluding] = useState<string | null>(null);
  const busy = useStoreBusy(params.organizationSlug, ref);
  const discarding = useStoreDiscarding(params.organizationSlug, ref);
  return (
    <>
      <BottomBar
        groups={groups}
        totalChanges={groups.reduce((count, group) => count + group.changeCount, 0)}
        runtimeChanges={diff.total_count + placements.count}
        notSet={store.services.reduce((count, service) => count + service.notSet.length, 0)}
        canSave={hasSomethingToSave}
        onDeploy={actions.deploy}
        admitting={actions.admitting}
        onSave={actions.save}
        // The Store goes first, at the version the user sees: a staged restore's own discard writes the Store too.
        onDiscardAll={() => placements.discardAll(() =>
          storeGroups.length > 0 || diff.total_count > 0 ? actions.discard(null, "review") : Promise.resolve(undefined))}
        onDiscardNode={discardNode}
        onDiscardRow={(group, path) => group.owner === "placement" ? placements.undo(group.nodeId, path)
          : actions.discard(path, group.rows.find((row) => row.path === path)?.discardTarget)}
        active={latest.filter((deployment) => deployment.in_flight)}
        ended={latest.filter((deployment) => !deployment.in_flight)}
        storage={storage}
        notes={storeHintNotes(diff, storeGroups, actions.neverSync)}
        proposals={{
          included: diff.included,
          onRemoveIncluded: ({ proposal }) => void writer.commit({ command: "remove_proposal", environment: ref, proposal, version: diff.version }),
          onIncludeNewer: ({ source }) => setIncluding(source.name),
        }}
        noServers={noServers}
        busy={busy}
        discarding={discarding}
      />
      {actions.dialog}
      {including === null ? null : (
        <Suspense fallback={null}>
          <SyncDialog organizationSlug={params.organizationSlug} from={{ project: ref.project, environment: including }}
            into={diff.environment.name} closable={() => false} onClose={() => setIncluding(null)} onSynced={() => setIncluding(null)} />
        </Suspense>
      )}
    </>
  );
}
