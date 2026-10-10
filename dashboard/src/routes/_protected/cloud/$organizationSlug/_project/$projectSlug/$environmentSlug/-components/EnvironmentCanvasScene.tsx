import { effectiveVolumes } from "#/modules/config-store/store-volumes";
import { replicaCount } from "#/modules/config-store/volume-sharing";
import { CANVAS_FIT_VIEW } from "./canvas/constants";
import { Suspense, useState } from "react";
import { Schema } from "effect";
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
import { buildStoreEdges, buildStoreNodes, volumeTrays } from "./canvas/nodes";
import type { StoreCanvas } from "./canvas/types";
import { configTrays } from "#/modules/config-store/store-configs";
import { branchQuery, configsQuery, diffQuery, domainsQuery, environmentSettingsQuery, namespaceQuery, requireView, servicesQuery, useStoreViews, volumesQuery } from "#/modules/config-store/store-view.queries";
import { liveNodes } from "#/modules/config-store/store-branches";
import { serviceChangeCount, serviceChanges, serviceSettingRows, settingText } from "#/modules/config-store/store-services";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";
import { StorePickingProvider } from "./new-branch/StoreNewBranchPanel";
import { StorePrPickingProvider } from "./pr-environments/StorePrPlanPanel";
import { RuntimeLensProvider } from "./canvas/RuntimeLensProvider";

/** A `replicas` Setting that says how many. */
const isReplicaCount = Schema.is(Schema.Int);

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
  const { store: ref, environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { organizationId } = useLoaderData({ from: "/_protected/cloud/$organizationSlug" });
  // The branch view is refused unless this is a Branch.
  const [servicesResult, settingsResult, diffResult, volumesResult, branch, namespace, domainsResult, configsResult] = useStoreViews(organizationSlug,
    [servicesQuery(ref), environmentSettingsQuery(ref), diffQuery(ref), volumesQuery(ref), branchQuery(ref), namespaceQuery(ref), domainsQuery(ref),
      configsQuery(ref)] as const);
  const { selectedNodeId } = useCanvasInspectorSelection();
  const positions = getCanvasPositionsCollection(organizationSlug, scope);
  const { data: positionRows } = useLiveSuspenseQuery({
    queryKey: ["canvas-positions", positions.id, environmentId],
    query: (q) => q.from({ position: positions }).where(({ position }) => eq(position.environmentId, environmentId))
      .select(({ position }) => position),
  });
  // A closed Branch is deleted under the open page; the page leaves for its Parent, the canvas just stops drawing.
  if ([servicesResult, settingsResult, diffResult, volumesResult, domainsResult, configsResult].some((r) => !r.ok && r.refusal.code === "not_found")) {
    return <PendingCanvas />;
  }
  const services = requireView(servicesResult);
  const settings = requireView(settingsResult);
  const diff = requireView(diffResult);
  const volumes = { ...requireView(volumesResult), volumes: effectiveVolumes(requireView(volumesResult).volumes, services.services, settings) };
  const domains = requireView(domainsResult).domains;
  const configListings = requireView(configsResult).configs;
  const configs = configTrays(services.services, configListings, diff);
  const canvasPositions = positionRows.map((row) => parseLiveQueryRow(canvasPositionSchema, row));
  const { trays, unmounted } = volumeTrays(services.services, volumes.volumes, diff,
    (name) => replicaCount(serviceSettingRows(settings, name).get("replicas")));
  const store: StoreCanvas = {
    services: services.services.map((service) => {
      const changes = serviceChanges(diff, service.id);
      // What runs asks for the deployed count, not one the next Deploy would set.
      const replicas = changes.get("replicas")?.before ?? serviceSettingRows(settings, service.name).get("replicas")?.value;
      // Its containers are named by the deployed private DNS until the Deploy that changes it lands.
      const privateDns = settingText(changes.get("privateDns")?.before) || service.private_dns;
      return {
        service,
        domains: domains.filter((domain) => domain.service === service.name),
        changeCount: serviceChangeCount(diff, service.id),
        runtimeIdentity: namespace.ok ? `${namespace.value.namespace}/${privateDns}` : null,
        desiredReplicas: isReplicaCount(replicas) ? replicas : null,
        trays: trays.get(service.id) ?? [],
        configTrays: configs.trays.get(service.id) ?? [],
      };
    }),
    volumes: volumes.volumes,
    configs: configListings,
    unmountedVolumes: unmounted,
    unmountedConfigs: configs.unmounted,
    live: branch.ok ? liveNodes(branch.value.live, services.services) : [],
    diff,
  };
  const initialNodes = buildStoreNodes(store, canvasPositions, environmentId);
  const initialEdges = buildStoreEdges(store);

  return (
    <ReactFlowProvider
      key={`${projectSlug}/${environmentSlug}`}
      initialNodes={initialNodes}
      initialEdges={initialEdges}
      initialWidth={1200}
      initialHeight={800}
      // A link to a node that isn't here (stale, deleted) shows the whole canvas, not an unfitted corner.
      fitView={!initialNodes.some((node) => node.id === selectedNodeId)}
      initialFitViewOptions={CANVAS_FIT_VIEW}
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
  const { selectedNodeId, selectedServiceId, deploymentId, deploymentReturnTo, deploymentList, newBranch, prPlan } = useCanvasInspectorSelection();
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
        : prPlan ? { key: `${canvasKey}/pr-plan`, nodeId: "pr-plan", picking: true } : null}
      canvas={<>
        {/* The live canvas stays mounted under a Deployment Page, which only lights up what it changed. */}
        <DeploymentLightingProvider value={lighting}>
          <Suspense fallback={<PendingCanvas />}>
            <RuntimeLensProvider organizationSlug={organizationSlug}>
              <CanvasWithData key={canvasKey} />
            </RuntimeLensProvider>
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
