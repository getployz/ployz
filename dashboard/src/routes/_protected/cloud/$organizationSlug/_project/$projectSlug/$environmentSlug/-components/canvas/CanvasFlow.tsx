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
import { useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import { PlusIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { prefersReducedMotion } from "#/lib/motion";
import { BottomBar } from "./BottomBar";
import { storeHintNotes } from "./store-hints";
import { CANVAS_MAX_ZOOM, CANVAS_MIN_ZOOM, SNAP_GRID } from "./constants";
import { canvasNodeTypes } from "./canvas-node-types";
import { CanvasNodeList } from "./CanvasServiceList";
import { useCanvasPositionMutation, useKeepPlacements } from "./useCanvasPositionMutation";
import { fitViewport, useCanvasNavigation } from "./useCanvasNavigation";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { useBlankServiceCreator, useOpenCreatedService } from "../../../../-components/service-create/useServiceCreator";
import { canvasNodeOf } from "./nodes";
import { ServiceEdge } from "./ServiceEdge";
import { CanvasKeepOut, RoutedStepEdge, useKeepOut, useRoutedEdges } from "#/modules/canvas/routed-edges";
import { LitVolumeProvider } from "./VolumeTray";
import { usePickingView } from "../new-branch/branch-picking";
import { useVolumeCreator } from "./useVolumeCreator";
import { useConfigCreator } from "./useConfigCreator";
import { useCanvasCreator } from "./useCanvasCreator";
import { ConfigCreatorDialog } from "./ConfigCreatorDialog";
import { useStoreChangeActions } from "./useStoreChangeActions";
import { useStoreDeployments } from "#/modules/config-store/store-view.queries";
import { deployWaits } from "#/modules/config-store/deploy-waits";
import { type ChangeGroup, reviewGroups } from "#/modules/config-store/store-deployments";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { mergeRestoreGroups } from "#/modules/volume-placement/volume-placement.review";
import { useStagedPlacements, useStorageSteps } from "#/modules/volume-placement/volume-placement.hooks";
import { deploymentDestination } from "#/modules/config-store/deployments/run-search";
import { CanvasContextMenu } from "../../../../-components/service-create/CanvasContextMenu";
import { useEnvironmentRenaming } from "../StoreRenameLock";
import { CanvasFinder } from "./CanvasFinder";
import { SyncButton } from "../sync/SyncButton";
import { SyncDialog } from "../sync/SyncDialog";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { useProtectedAsk } from "#/modules/config-store/use-protected-ask";
import { useStoreBusy, useStoreDiscarding } from "#/modules/config-store/store-pending";
import { ServiceCreatorDialog } from "./ServiceCreatorDialog";
import { VolumeCreatorDialog } from "./VolumeCreatorDialog";
import { useKeyboardFocusModality } from "../keyboard-focus-modality";
import { RuntimeLensContext } from "./RuntimeLensProvider";
import { ENVIRONMENT_ROUTE_FROM } from "#/lib/environment-routes";
import type { CanvasResourceNode, StoreCanvas } from "./types";
import type { CanvasPlacement } from "#/modules/canvas/canvas-positions";

// A Live Node's link, dashed by `buildStoreEdges`, and a Service's, solid by `buildServiceEdges`; each is routed around
// the cards and the floating bars, ending in an arrow.
const DEFAULT_EDGE_OPTIONS = {
  markerEnd: { type: MarkerType.ArrowClosed, color: "var(--primary)" },
} as const;

const canvasEdgeTypes = { service: ServiceEdge, routed: RoutedStepEdge };

export function CanvasFlow({
  organizationId,
  environmentId,
  canvasNodes,
  canvasEdges,
  placements,
  store,
}: {
  organizationId: string;
  environmentId: string;
  canvasNodes: CanvasResourceNode[];
  canvasEdges: Edge[];
  /** The nodes drawn without a stored place, where they are drawn; the canvas keeps those places. */
  placements: readonly CanvasPlacement[];
  store: StoreCanvas;
}) {
  const [flowReady, setFlowReady] = useState(false);
  // A Service link says why it exists while hovered, and stays saying it once clicked, until the pane is.
  const [hoveredEdge, setHoveredEdge] = useState<string | null>(null);
  const [pickedEdge, setPickedEdge] = useState<string | null>(null);
  const { keepOut, keepOutRef } = useKeepOut();
  const edges = useRoutedEdges(canvasEdges, keepOut).map((edge) => edge.type === "service" && (edge.id === hoveredEdge || edge.id === pickedEdge)
    ? { ...edge, data: { ...edge.data, shown: true } } : edge);
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { onNodeDrag } = useCanvasPositionMutation({
    ...params,
    organizationId,
  });
  useKeepPlacements(params.organizationSlug, environmentId, placements);
  const { selectedNodeId } = useCanvasInspectorSelection();
  const renaming = useEnvironmentRenaming();
  const findableNodes = [...store.services.map(({ service }) => ({ id: service.id, name: service.name, type: "service" as const })),
    ...store.volumes.map((volume) => ({ id: volume.id, name: volume.name, type: "volume" as const })),
    ...store.configs.map((config) => ({ id: config.id, name: config.name, type: "config" as const }))];
  // A Volume in a tray shows on its Service's node, so that is the node the canvas brings into view.
  const focusNodeId = selectedNodeId === null ? null : canvasNodeOf(canvasNodes, selectedNodeId) ?? selectedNodeId;
  const focusNode = canvasNodes.find((node) => node.id === focusNodeId);
  const focusPositionKey = focusNode ? `${focusNode.position.x}:${focusNode.position.y}` : null;
  const picking = usePickingView();
  const { getViewportCenter } = useCanvasNavigation(
    focusNodeId,
    focusPositionKey,
    flowReady,
    // Picking a Branch brings the whole canvas into view beside the panel.
    picking ? { key: "new-branch", nodeIds: canvasNodes.map((node) => node.id) } : null,
  );
  useKeyboardFocusModality();
  useRefitOnWidthChange(flowReady && !focusNode);
  const creator = useCanvasCreator(getViewportCenter);
  const openCreated = useOpenCreatedService(params);
  const createBlankService = useBlankServiceCreator(params, environmentId);
  const createVolume = useVolumeCreator(params.organizationSlug, environmentId);
  const createConfig = useConfigCreator(params, environmentId);

  return (
    <CanvasKeepOut value={keepOutRef}>
    <LitVolumeProvider>
      <div className="canvas-graph aria-busy:opacity-60" inert={selectedNodeId !== null || renaming} aria-busy={renaming}>
      <div className="hidden h-full min-[861px]:block">
        <CanvasContextMenu
          onCreateFromPanel={(panel) => creator.openAtLastRightClick("service", panel)}
          onCreateBlank={() => createBlankService(creator.placeAtLastRightClick())}
          onCreateVolume={() => creator.openAtLastRightClick("volume")}
          onCreateConfig={() => creator.openAtLastRightClick("config")}
        >
          <ReactFlow
            key={`${params.projectSlug}/${params.environmentSlug}`}
            nodes={canvasNodes}
            edges={edges}
            defaultEdgeOptions={DEFAULT_EDGE_OPTIONS}
            edgeTypes={canvasEdgeTypes}
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
            onPaneContextMenu={creator.onPaneContextMenu}
            onEdgeMouseEnter={(_, edge) => setHoveredEdge(edge.id)}
            onEdgeMouseLeave={() => setHoveredEdge(null)}
            onEdgeClick={(_, edge) => setPickedEdge(edge.type === "service" && edge.id !== pickedEdge ? edge.id : null)}
            onPaneClick={() => setPickedEdge(null)}
          >
            <Background variant={BackgroundVariant.Dots} gap={16} size={1} />
          </ReactFlow>
        </CanvasContextMenu>
      </div>
      <CanvasNodeList store={store} selectedNodeId={selectedNodeId} />
      <div ref={keepOutRef} className="pointer-events-none absolute top-4 right-4 flex items-center gap-2">
        <SyncButton />
        <CanvasFinder nodes={findableNodes} />
        <Button
          className="pointer-events-auto"
          onClick={() => creator.openAtCenter("service")}
        >
          <PlusIcon data-icon="inline-start" />
          Create
        </Button>
      </div>
      </div>

      <StoreBottomBar store={store} />

      <ServiceCreatorDialog
        open={creator.open === "service"}
        onOpenChange={(open) => creator.setOpen("service", open)}
        panel={creator.panel}
        position={creator.position}
        params={params}
        onCreateVolume={() => creator.switchTo("volume")}
        onCreateConfig={() => creator.switchTo("config")}
        onCreated={async (created, stillHere) => {
          creator.setOpen("service", false);
          if (stillHere) await openCreated(created);
        }}
      />
      <VolumeCreatorDialog
        organizationSlug={params.organizationSlug}
        open={creator.open === "volume"}
        onOpenChange={(open) => creator.setOpen("volume", open)}
        position={creator.position}
        onCreate={createVolume}
      />
      <ConfigCreatorDialog open={creator.open === "config"} onOpenChange={(open) => creator.setOpen("config", open)}
        onCreate={(name) => createConfig(name, creator.position)} />
    </LitVolumeProvider>
    </CanvasKeepOut>
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
    (deploymentId) => void navigate(deploymentDestination(params, deploymentId)), placements.loss);
  const deployments = useStoreDeployments(params.organizationSlug, ref).data.pages[0];
  const { noServers } = use(RuntimeLensContext);
  const { machines, volumeCopies } = useRuntimeLens(params.organizationSlug);
  const storage = useStorageSteps(params.organizationSlug);
  const storeGroups = reviewGroups(diff, store.services.map(({ service }) => service), store.region);
  const waits = deployWaits({
    services: store.services.map(({ service, replicas }) => ({ name: service.name, replicas })),
    home: store.region, volumes: store.volumes, copies: volumeCopies, servers: machines,
  });
  const groups = mergeRestoreGroups(storeGroups, placements.groups);
  const discardNode = (group: ChangeGroup) => group.owner === "placement" ? placements.discard(group.nodeId) : actions.discard(group.discardPath, group.discardTarget);
  const draftCount = diff.draft_count ?? (diff.holds_staged ? 0 : diff.total_count);
  const hasSomethingToSave = draftCount > 0 || diff.included.length > 0;
  const writer = useStoreWriter(params.organizationSlug);
  const [including, setIncluding] = useState<string | null>(null);
  const busy = useStoreBusy(params.organizationSlug, ref);
  const discarding = useStoreDiscarding(params.organizationSlug, ref);
  const asks = useProtectedAsk(params.organizationSlug, ref);
  return (
    <>
      <BottomBar
        groups={groups}
        totalChanges={groups.reduce((count, group) => count + group.changeCount, 0)}
        stagedChanges={placements.groups.reduce((count, group) => count + group.changeCount, draftCount)}
        waits={waits}
        undeployedSave={draftCount === 0 && diff.total_count > 0 ? diff.saved : null}
        ranNothingSave={diff.saved_ran_nothing ? diff.saved : null}
        notSet={store.services.reduce((count, service) => count + service.notSet.length, 0)}
        canSave={hasSomethingToSave}
        onDeploy={actions.deploy}
        onSave={actions.save}
        // The Store goes first, at the version the user sees: a staged restore's own discard writes the Store too.
        onDiscardAll={() => placements.discardAll(() =>
          storeGroups.length > 0 || diff.total_count > 0 ? actions.discard(null, "review") : Promise.resolve(undefined))}
        onDiscardNode={discardNode}
        onDiscardRow={(group, path) => group.owner === "placement" ? placements.undo(group.nodeId, path)
          : actions.discard(path, group.rows.find((row) => row.path === path)?.discardTarget)}
        active={deployments?.in_flight ?? []}
        latest={deployments?.latest}
        storage={storage}
        notes={storeHintNotes(diff, storeGroups, actions.neverSync)}
        proposals={{
          included: diff.included,
          onRemoveIncluded: ({ proposal }) => void writer.commit({ command: "remove_proposal", environment: ref, proposal, version: diff.version }),
          onIncludeNewer: ({ source }) => setIncluding(source.name),
        }}
        noServers={noServers}
        asks={asks}
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
