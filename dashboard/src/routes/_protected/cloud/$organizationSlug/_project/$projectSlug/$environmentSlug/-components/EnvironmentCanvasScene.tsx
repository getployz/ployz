import { useCollectionScope } from "#/collections/use-collection-scope";
import { getEnvironmentDocumentsCollection, useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { Suspense, useState } from "react";
import {
  Background,
  BackgroundVariant,
  ReactFlow,
  ReactFlowProvider,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { eq, inArray, useLiveQuery, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useLiveNodes } from "#/modules/branches/use-live-nodes";
import { Outlet, useLoaderData, useParams } from "@tanstack/react-router";
import { parseLiveQueryRow } from "#/lib/tanstack-db";
import {
  buildEnvironmentServicesViewQuery,
  normalizeEnvironmentServicesViewRecord,
} from "#/modules/services/services.collection";
import { useEnvironmentChangeStateProjection } from "#/modules/deployments/environment-change-state.queries";
import { getEnvironmentNodeIntroductionsCollection } from "#/collections/collections";
import { environmentNodeIntroductionSchema } from "#/modules/environment-design/environment-node-introductions";
import {
  environmentResourceCanvasPositionSchema,
  volumeResourceRecordSchema,
} from "#/modules/environment-design/resources";
import {
  useCanvasPositionsCollection,
  useServicesCollection,
  useVolumeResourcesCollection,
} from "#/modules/services/services.collection";
import { CanvasInspectorOverlay } from "./CanvasInspectorOverlay";
import { useCanvasInspectorSelection } from "./useCanvasInspectorSelection";
import { LOADING_NODE, canvasNodeTypes } from "./canvas/canvas-node-types";
import { BottomBarSlot } from "./canvas/BottomBar";
import { CanvasFlow } from "./canvas/CanvasFlow";
import { DeploymentLightingProvider, useOpenDeployment } from "./deployment-page";
import { buildEdges, buildLiveEdges, buildLiveNodes, buildNodes } from "./canvas/nodes";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";
import { BranchPickingProvider } from "./new-branch/branch-picking";

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

function CanvasWithData() {
  const collectionScope = useCollectionScope();
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId, organizationId } = useLoaderData({
    from: ENVIRONMENT_ROUTE_FROM,
  });
  const servicesCollection = useServicesCollection(params.organizationSlug);
  const canvasPositionsCollection = useCanvasPositionsCollection(
    params.organizationSlug,
  );
  const volumeResourcesCollection = useVolumeResourcesCollection(
    params.organizationSlug,
  );
  const documents = getEnvironmentDocumentsCollection(params.organizationSlug, collectionScope);
  const document = useEnvironmentDocument(params.organizationSlug, environmentId);
  const nodeIntroductionsCollection = getEnvironmentNodeIntroductionsCollection(
    params.organizationSlug, collectionScope,
  );
  const environmentChangeState = useEnvironmentChangeStateProjection({
    organizationSlug: params.organizationSlug,
    environmentId,
  });
  const { selectedNodeId } = useCanvasInspectorSelection();
  const { data: services } = useLiveSuspenseQuery({
    queryKey: ['canvas-services', servicesCollection.id, canvasPositionsCollection.id, documents.id, params.projectSlug, params.environmentSlug],
    query: (q) =>
      buildEnvironmentServicesViewQuery(q, params, {
        services: servicesCollection,
        canvasPositions: canvasPositionsCollection,
        documents,
      }),
  });
  const { data: canvasPositionRows } = useLiveSuspenseQuery({
    queryKey: ['canvas-positions', canvasPositionsCollection.id, environmentId],
    query: (q) =>
      q
        .from({ canvasPosition: canvasPositionsCollection })
        .where(({ canvasPosition }) =>
          eq(canvasPosition["environmentId"], environmentId),
        )
        .select(({ canvasPosition }) => ({
          id: canvasPosition["id"],
          environmentId: canvasPosition["environmentId"],
          resourceType: canvasPosition["resourceType"],
          resourceId: canvasPosition["resourceId"],
          x: canvasPosition["x"],
          y: canvasPosition["y"],
          createdAt: canvasPosition["createdAt"],
          updatedAt: canvasPosition["updatedAt"],
        })),
  });
  const { data: volumeResourceRows } = useLiveSuspenseQuery({
    queryKey: ['canvas-volumes', volumeResourcesCollection.id, params.projectSlug, params.environmentSlug],
    query: (q) =>
      q
        .from({ resource: volumeResourcesCollection })
        .where(({ resource }) => eq(resource.projectSlug, params.projectSlug))
        .where(({ resource }) =>
          eq(resource.environmentSlug, params.environmentSlug),
        )
        .select(({ resource }) => resource),
  });
  const { data: nodeIntroductionRows } = useLiveSuspenseQuery({
    queryKey: ['canvas-introductions', nodeIntroductionsCollection.id, environmentId],
    query: (q) =>
      q
        .from({ introduction: nodeIntroductionsCollection })
        .where(({ introduction }) =>
          eq(introduction.environmentId, environmentId),
        )
        .select(({ introduction }) => ({
          organizationId: introduction.organizationId,
          environmentId: introduction.environmentId,
          nodeType: introduction.nodeType,
          nodeId: introduction.nodeId,
          nodeLineageId: introduction.nodeLineageId,
          configVersion: introduction.configVersion,
          config: introduction.config,
          createdAt: introduction.createdAt,
          updatedAt: introduction.updatedAt,
        })),
  });
  // A Branch draws the services its Own Copies use live where they sit on their owner's canvas.
  const liveNodes = useLiveNodes(params.organizationSlug, environmentId);
  const liveOwnerNodeIds = liveNodes.flatMap((node) => node.owner ? [node.owner.node.nodeId] : []);
  const { data: liveNodePositionRows } = useLiveQuery({
    queryKey: ['canvas-live-positions', canvasPositionsCollection.id, liveOwnerNodeIds.join()],
    query: (q) => q.from({ canvasPosition: canvasPositionsCollection })
      .where(({ canvasPosition }) => inArray(canvasPosition["resourceId"], liveOwnerNodeIds))
      .select(({ canvasPosition }) => ({
        id: canvasPosition["id"], environmentId: canvasPosition["environmentId"], resourceType: canvasPosition["resourceType"],
        resourceId: canvasPosition["resourceId"], x: canvasPosition["x"], y: canvasPosition["y"],
        createdAt: canvasPosition["createdAt"], updatedAt: canvasPosition["updatedAt"],
      })),
  });
  const liveNodePositions = liveNodePositionRows.map((position) => parseLiveQueryRow(environmentResourceCanvasPositionSchema, position));
  const nodeIntroductions = nodeIntroductionRows.map((introduction) =>
    parseLiveQueryRow(environmentNodeIntroductionSchema, introduction),
  );
  const volumeResources = volumeResourceRows.map((resource) =>
    parseLiveQueryRow(volumeResourceRecordSchema, resource),
  );
  const canvasPositions = canvasPositionRows.map((position) =>
    parseLiveQueryRow(environmentResourceCanvasPositionSchema, position),
  );
  const servicesWithBoundEnv = services.map(normalizeEnvironmentServicesViewRecord);
  const serviceVolumeAttachments = document?.intent.services.flatMap((service) =>
    service.volumeAttachments.map((attachment) => ({ ...attachment, serviceId: service.id, environmentId }))) ?? [];
  const activeServicesWithBoundEnv = servicesWithBoundEnv.filter(
    (service) => service.service.deletedAt == null,
  );
  const initialNodes = [...buildNodes(
    activeServicesWithBoundEnv,
    canvasPositions,
    selectedNodeId,
    volumeResources,
  ), ...buildLiveNodes(liveNodes, liveNodePositions).map((node) => ({ ...node, selected: node.id === selectedNodeId }))];
  const initialEdges = [...buildEdges(
    volumeResources,
    serviceVolumeAttachments,
    activeServicesWithBoundEnv,
  ), ...buildLiveEdges(liveNodes)];

  return (
    <ReactFlowProvider
      key={`${params.projectSlug}/${params.environmentSlug}`}
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
        servicesWithBoundEnv={servicesWithBoundEnv}
        volumeResources={volumeResources}
        environmentChangeState={environmentChangeState}
        nodeIntroductions={nodeIntroductions}
        canvasNodes={initialNodes}
        canvasEdges={initialEdges}
      />
    </ReactFlowProvider>
  );
}

export function EnvironmentCanvasScene() {
  const { organizationSlug, projectSlug, environmentSlug } = useParams({
    from: ENVIRONMENT_ROUTE_FROM,
  });
  const canvasKey = `${organizationSlug}/${projectSlug}/${environmentSlug}`;
  const { selectedNodeId, selectedServiceId, deploymentId, deploymentReturnTo, deploymentList, newBranch } = useCanvasInspectorSelection();
  const lighting = useOpenDeployment();
  const [bottomBarSlot, setBottomBarSlot] = useState<HTMLElement | null>(null);

  return (
    <BottomBarSlot.Provider value={bottomBarSlot}>
    <BranchPickingProvider open={newBranch !== null} focus={newBranch?.focus ?? null}>
    <CanvasInspectorOverlay
      selection={selectedNodeId ? {
        key: `${canvasKey}/${selectedServiceId ? "service" : "resource"}/${selectedNodeId}`,
        nodeId: selectedNodeId,
      } : deploymentId ? { key: `${canvasKey}/deployment/${deploymentId}`, nodeId: deploymentId, lit: true, returnTo: deploymentReturnTo }
        : deploymentList ? { key: `${canvasKey}/deployments`, nodeId: "deployments" }
        : newBranch ? { key: `${canvasKey}/new-branch`, nodeId: "new-branch", picking: true } : null}
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
    </BranchPickingProvider>
    </BottomBarSlot.Provider>
  );
}
