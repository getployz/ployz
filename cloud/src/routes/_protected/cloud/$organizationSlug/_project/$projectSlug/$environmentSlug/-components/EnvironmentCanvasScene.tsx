import { useCollectionScope } from "#/collections/use-collection-scope";
import { getEnvironmentDocumentsCollection, useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { Suspense, useRef } from "react";
import {
  Background,
  BackgroundVariant,
  ReactFlow,
  ReactFlowProvider,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { Outlet, useLoaderData, useParams } from "@tanstack/react-router";
import { parseLiveQueryRow } from "#/lib/tanstack-db";
import {
  buildEnvironmentServicesViewQuery,
  normalizeEnvironmentServicesViewRecord,
} from "#/modules/services/services.collection";
import { useEnvironmentChangeStateProjection } from "#/modules/deployments/use-environment-state-projection";
import { getEnvironmentNodeIntroductionsCollection } from "#/collections/collections";
import { environmentNodeIntroductionSchema } from "#/modules/environment-design/environment-node-introductions";
import {
  environmentResourceCanvasPositionSchema,
  variableGroupResourceRecordSchema,
  volumeResourceRecordSchema,
} from "#/modules/environment-design/resources";
import {
  useCanvasPositionsCollection,
  useEnvironmentResourcesCollection,
  useServicesCollection,
  useVolumeResourcesCollection,
} from "#/modules/services/services.collection";
import { CanvasInspectorOverlay } from "./CanvasInspectorOverlay";
import { useCanvasInspectorSelection } from "./useCanvasInspectorSelection";
import { LOADING_NODE, canvasNodeTypes } from "./canvas/canvas-node-types";
import { CanvasFlow } from "./canvas/CanvasFlow";
import { buildEdges, buildNodes } from "./canvas/nodes";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";

export function PendingCanvas() {
  return (
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
  );
}

function CanvasWithData() {
  const collectionScope = useCollectionScope();
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId, organizationId } = useLoaderData({
    from: ENVIRONMENT_ROUTE_FROM,
  });
  const servicesCollection = useServicesCollection(params.organizationSlug);
  const environmentResourcesCollection = useEnvironmentResourcesCollection(
    params.organizationSlug,
  );
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
    query: (q) =>
      buildEnvironmentServicesViewQuery(q, params, {
        services: servicesCollection,
        canvasPositions: canvasPositionsCollection,
        documents,
      }),
  });
  const { data: environmentResourceRows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ resource: environmentResourcesCollection })
        .where(({ resource }) => eq(resource.projectSlug, params.projectSlug))
        .where(({ resource }) =>
          eq(resource.environmentSlug, params.environmentSlug),
        )
        .select(({ resource }) => resource),
  });
  const { data: canvasPositionRows } = useLiveSuspenseQuery({
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
  const nodeIntroductions = nodeIntroductionRows.map((introduction) =>
    parseLiveQueryRow(environmentNodeIntroductionSchema, introduction),
  );
  const environmentResources = environmentResourceRows.map((resource) =>
    parseLiveQueryRow(variableGroupResourceRecordSchema, resource),
  );
  const volumeResources = volumeResourceRows.map((resource) =>
    parseLiveQueryRow(volumeResourceRecordSchema, resource),
  );
  const canvasPositions = canvasPositionRows.map((position) =>
    parseLiveQueryRow(environmentResourceCanvasPositionSchema, position),
  );
  const servicesWithBoundEnv = services.map(normalizeEnvironmentServicesViewRecord);
  const serviceVariableGroupAttachments = document?.intent.services.flatMap((service) =>
    service.variableGroupAttachments.map((attachment) => ({ ...attachment, serviceId: service.id, environmentId }))) ?? [];
  const serviceVolumeAttachments = document?.intent.services.flatMap((service) =>
    service.volumeAttachments.map((attachment) => ({ ...attachment, serviceId: service.id, environmentId }))) ?? [];
  const activeServicesWithBoundEnv = servicesWithBoundEnv.filter(
    (service) => service.service.deletedAt == null,
  );
  const initialNodes = buildNodes(
    activeServicesWithBoundEnv,
    environmentResources,
    canvasPositions,
    selectedNodeId,
    volumeResources,
  );
  const initialEdges = buildEdges(
    environmentResources,
    serviceVariableGroupAttachments,
    volumeResources,
    serviceVolumeAttachments,
    activeServicesWithBoundEnv,
  );

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
        environmentResources={environmentResources}
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
  const servicesRef = useRef<HTMLDivElement>(null);
  const { projectSlug, environmentSlug } = useParams({
    from: ENVIRONMENT_ROUTE_FROM,
  });
  const canvasKey = `${projectSlug}/${environmentSlug}`;
  const { isInspectorOpen: isInspectorPage } = useCanvasInspectorSelection();

  return (
    <div
      ref={servicesRef}
      role="region"
      aria-label="Environment services"
      tabIndex={0}
      className="relative h-full w-full outline-none"
    >
      <Suspense fallback={<PendingCanvas />}>
        <CanvasWithData key={canvasKey} />
      </Suspense>
      {isInspectorPage ? (
        <CanvasInspectorOverlay finalFocus={servicesRef}>
          <Outlet />
        </CanvasInspectorOverlay>
      ) : null}
    </div>
  );
}
