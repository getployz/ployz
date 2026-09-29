import { Suspense, useState } from "react";
import {
  Background,
  BackgroundVariant,
  ReactFlow,
  ReactFlowProvider,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { Outlet, useLoaderData, useParams } from "@tanstack/react-router";
import { getCanvasPositionsCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { parseLiveQueryRow } from "#/lib/tanstack-db";
import { canvasPositionSchema } from "#/modules/canvas/canvas-positions";
import { CanvasInspectorOverlay } from "./CanvasInspectorOverlay";
import { useCanvasInspectorSelection } from "./useCanvasInspectorSelection";
import { LOADING_NODE, canvasNodeTypes } from "./canvas/canvas-node-types";
import { BottomBarSlot } from "./canvas/BottomBar";
import { CanvasFlow } from "./canvas/CanvasFlow";
import { DeploymentLightingProvider, useOpenDeployment } from "./deployment-page";
import { buildStoreEdges, buildStoreNodes } from "./canvas/nodes";
import type { StoreCanvasService } from "./canvas/types";
import { branchQuery, diffQuery, environmentSettingsQuery, requireView, servicesQuery, useStoreView, volumesQuery } from "#/modules/config-store/store-view.queries";
import { liveNodes } from "#/modules/config-store/store-branches";
import { serviceChanges, serviceSettingRows, settingText } from "#/modules/config-store/store-services";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";
import { StorePickingProvider } from "./new-branch/StoreNewBranchPanel";
import { StorePrPickingProvider } from "./pr-environments/StorePrPlanPanel";

export function PendingCanvas() {
  return (
    <div className="canvas-graph">
    <ReactFlowProvider
      initialNodes={[LOADING_NODE]}
      initialWidth={1200}
      initialHeight={800}
      fitView
      initialMaxZoom={1.25}
    >
      <ReactFlow
        nodes={[LOADING_NODE]}
        edges={[]}
        nodeTypes={canvasNodeTypes}
        width={1200}
        height={800}
        fitView
        proOptions={{ hideAttribution: true }}
        nodesDraggable={false}
        nodesConnectable={false}
        panOnDrag={false}
        zoomOnScroll={false}
      >
        <Background variant={BackgroundVariant.Dots} gap={16} size={1} />
      </ReactFlow>
    </ReactFlowProvider>
    </div>
  );
}

/** The canvas over the Config Store: its Services, Volumes and Live Nodes, where the canvas last put them. */
function CanvasWithData() {
  const scope = useCollectionScope();
  const { organizationSlug, projectSlug, environmentSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store: ref, environmentId, organizationId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const services = requireView(useStoreView(organizationSlug, servicesQuery(ref)));
  const settings = requireView(useStoreView(organizationSlug, environmentSettingsQuery(ref)));
  const diff = requireView(useStoreView(organizationSlug, diffQuery(ref)));
  const volumes = requireView(useStoreView(organizationSlug, volumesQuery(ref)));
  // Refused unless this is a Branch.
  const branch = useStoreView(organizationSlug, branchQuery(ref));
  const { selectedNodeId } = useCanvasInspectorSelection();
  const positions = getCanvasPositionsCollection(organizationSlug, scope);
  const { data: positionRows } = useLiveSuspenseQuery({
    queryKey: ["canvas-positions", positions.id, environmentId],
    query: (q) => q.from({ position: positions }).where(({ position }) => eq(position.environmentId, environmentId))
      .select(({ position }) => position),
  });
  const canvasPositions = positionRows.map((row) => parseLiveQueryRow(canvasPositionSchema, row));
  const store = {
    services: services.services.map((service): StoreCanvasService => ({
      service,
      subtitle: service.source === "uploaded" ? "Uploaded"
        : settingText(serviceSettingRows(settings, service.name).get(service.source === "git" ? "repository" : "image")?.value) || null,
      changeCount: serviceChanges(diff, service.id).size,
    })),
    volumes: volumes.volumes,
    live: branch.ok ? liveNodes(branch.value.live, settings, services.services) : [],
    diff,
  };
  const initialNodes = buildStoreNodes(store, canvasPositions, selectedNodeId, environmentId);
  const initialEdges = buildStoreEdges(store);

  return (
    <ReactFlowProvider
      key={`${projectSlug}/${environmentSlug}`}
      initialNodes={initialNodes}
      initialEdges={initialEdges}
      initialWidth={1200}
      initialHeight={800}
      fitView={!selectedNodeId}
      initialMaxZoom={1.25}
    >
      <CanvasFlow
        organizationId={organizationId}
        environmentId={environmentId}
        canvasNodes={initialNodes}
        canvasEdges={initialEdges}
        store={store}
      />
    </ReactFlowProvider>
  );
}

export function EnvironmentCanvasScene() {
  const { organizationSlug, projectSlug, environmentSlug } = useParams({
    from: ENVIRONMENT_ROUTE_FROM,
  });
  const canvasKey = `${organizationSlug}/${projectSlug}/${environmentSlug}`;
  const { selectedNodeId, selectedServiceId, deploymentId, deploymentReturnTo, deploymentList, newBranch, branchReview, prPlan } = useCanvasInspectorSelection();
  const lighting = useOpenDeployment();
  const [bottomBarSlot, setBottomBarSlot] = useState<HTMLElement | null>(null);

  return (
    <BottomBarSlot.Provider value={bottomBarSlot}>
    <StorePickingProvider newBranch={newBranch}>
    <StorePrPickingProvider prPlan={prPlan}>
    <CanvasInspectorOverlay
      selection={selectedNodeId ? {
        key: `${canvasKey}/${selectedServiceId ? "service" : "resource"}/${selectedNodeId}`,
        nodeId: selectedNodeId,
      } : deploymentId ? { key: `${canvasKey}/deployment/${deploymentId}`, nodeId: deploymentId, lit: true, returnTo: deploymentReturnTo }
        : deploymentList ? { key: `${canvasKey}/deployments`, nodeId: "deployments" }
        : newBranch ? { key: `${canvasKey}/new-branch`, nodeId: "new-branch", picking: true }
        : branchReview ? { key: `${canvasKey}/review`, nodeId: "review" }
        : prPlan ? { key: `${canvasKey}/pr-plan`, nodeId: "pr-plan", picking: true } : null}
      canvas={<>
        {/* The live canvas stays mounted under a Deployment Page, which only lights up what it changed. */}
        <DeploymentLightingProvider value={lighting}>
          <Suspense fallback={<PendingCanvas />}>
            <CanvasWithData key={canvasKey} />
          </Suspense>
        </DeploymentLightingProvider>
        <div ref={setBottomBarSlot} className="contents" />
      </>}
    >
      <Outlet />
    </CanvasInspectorOverlay>
    </StorePrPickingProvider>
    </StorePickingProvider>
    </BottomBarSlot.Provider>
  );
}
