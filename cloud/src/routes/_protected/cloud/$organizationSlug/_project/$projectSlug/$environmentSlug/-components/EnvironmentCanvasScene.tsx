import { Suspense } from "react";
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
  projectServiceViewsWithBoundEnv,
  projectServiceViewsWithBoundMounts,
} from "#/modules/services/services.collection";
import { useEnvironmentChangeStateProjection } from "#/modules/deployments/use-environment-state-projection";
import { getEnvironmentNodeIntroductionsCollection } from "#/electric/collections";
import { environmentNodeIntroductionSchema } from "#/modules/environment-design/environment-node-introductions";
import {
  environmentResourceCanvasPositionSchema,
  variableGroupResourceRecordSchema,
  volumeResourceRecordSchema,
} from "#/modules/environment-design/resources";
import { environmentServiceVolumeAttachmentSchema } from "#/modules/environment-design/service-volume-attachments";
import { environmentServiceVariableGroupAttachmentSchema } from "#/modules/environment-design/variables";
import {
  useCanvasPositionsCollection,
  useEnvironmentResourcesCollection,
  useServiceVariableGroupAttachmentsCollection,
  useServiceVolumeAttachmentsCollection,
  useServicesCollection,
  useVariablesCollection,
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
  const serviceVariableGroupAttachmentsCollection =
    useServiceVariableGroupAttachmentsCollection(params.organizationSlug);
  const volumeResourcesCollection = useVolumeResourcesCollection(
    params.organizationSlug,
  );
  const serviceVolumeAttachmentsCollection =
    useServiceVolumeAttachmentsCollection(params.organizationSlug);
  const variablesCollection = useVariablesCollection(params.organizationSlug);
  const nodeIntroductionsCollection = getEnvironmentNodeIntroductionsCollection(
    params.organizationSlug,
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
        variables: variablesCollection,
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
  const { data: serviceVariableGroupAttachmentRows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ attachment: serviceVariableGroupAttachmentsCollection })
        .where(({ attachment }) =>
          eq(attachment["environmentId"], environmentId),
        )
        .select(({ attachment }) => attachment),
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
  const { data: serviceVolumeAttachmentRows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ attachment: serviceVolumeAttachmentsCollection })
        .where(({ attachment }) =>
          eq(attachment["environmentId"], environmentId),
        )
        .select(({ attachment }) => attachment),
  });
  const { data: nodeIntroductionRows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ introduction: nodeIntroductionsCollection })
        .where(({ introduction }) =>
          eq(introduction.environmentId, environmentId),
        )
        .select(({ introduction }) => ({
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
  const normalizedServices = services.map(
    normalizeEnvironmentServicesViewRecord,
  );
  const serviceVariableGroupAttachments =
    serviceVariableGroupAttachmentRows.map((attachment) =>
      parseLiveQueryRow(
        environmentServiceVariableGroupAttachmentSchema,
        attachment,
      ),
    );
  const serviceVolumeAttachments = serviceVolumeAttachmentRows.map((attachment) =>
    parseLiveQueryRow(environmentServiceVolumeAttachmentSchema, attachment),
  );
  const servicesWithBoundEnv = projectServiceViewsWithBoundMounts({
    services: projectServiceViewsWithBoundEnv({
      services: normalizedServices,
      environmentResources,
      attachments: serviceVariableGroupAttachments,
    }),
    volumeResources,
    attachments: serviceVolumeAttachments,
  });
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
  const { projectSlug, environmentSlug } = useParams({
    from: ENVIRONMENT_ROUTE_FROM,
  });
  const canvasKey = `${projectSlug}/${environmentSlug}`;
  const { isInspectorOpen: isInspectorPage } = useCanvasInspectorSelection();

  return (
    <div className="relative h-full w-full">
      <Suspense key={canvasKey} fallback={<PendingCanvas />}>
        <CanvasWithData key={canvasKey} />
      </Suspense>
      {isInspectorPage ? (
        <CanvasInspectorOverlay>
          <Outlet />
        </CanvasInspectorOverlay>
      ) : null}
    </div>
  );
}
