import { useServiceMetadataEditor } from "#/modules/environment-design/service-metadata.collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getEnvironmentDocumentsCollection } from "#/modules/environment-design/environment-document.collection";
import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { redirect, useMatch } from "@tanstack/react-router";
import {
  ENVIRONMENT_INDEX_ROUTE_TO,
  ENVIRONMENT_ROUTE_FROM,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";
import { DEFAULT_SERVICE_PORT, parseServiceConfig } from "@ployz/sdk/config";
import {
  getServiceDeploymentDiffState,
  type ServiceDeploymentDiffState,
} from "#/modules/services/service-deployment-diff/state";
import { buildEnvironmentNodeChange, type EnvironmentNodeProjection } from "#/modules/environment-design/environment-change-set";
import { projectServiceDeploymentConfig } from "#/modules/environment-design/services";
import {
  buildEnvironmentServicesViewQuery,
  type ServiceWriter,
  type EnvironmentServiceViewRecord,
  normalizeEnvironmentServicesViewRecord,
  useCanvasPositionsCollection,
  useServicesCollection,
  useServiceWriter,
} from "#/modules/services/services.collection";
import { useEnvironmentChangeStateProjection } from "#/modules/deployments/environment-change-state.queries";
import type { EnvironmentNodeNameIdentity } from "#/modules/environment-design/environment-node-names";
import { getEnvironmentNodeIntroductionsCollection } from "#/collections/collections";
import { environmentNodeIntroductionSchema } from "#/modules/environment-design/environment-node-introductions";
import { decodeStrict } from "#/modules/environment-design/schema";

export type ServiceRouteParams = {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
  serviceId: string;
};

export type ServiceDrawerState = {
  organizationSlug: string;
  environmentSlug: string;
  service: EnvironmentServiceViewRecord["service"];
  environmentNodes: EnvironmentNodeNameIdentity[];
  diff: ServiceDeploymentDiffState;
  collection: ServiceWriter;
  editMetadata: (input: Parameters<ReturnType<typeof useServiceMetadataEditor>>[0]) => { isPersisted: { promise: Promise<unknown> } };
  /** Managed-domain prefixes already claimed by other services in this
   * environment, for client-side uniqueness hints (server validates org-wide). */
  managedPrefixesInUse: string[];
  /** Advisory PORT hint. Null means the authored PORT is not a known valid literal. */
  defaultTargetPort: number | null;
  /** Public domains in the Service's Applied State; empty before its first deploy. */
  appliedDomains: { managedPrefixes: ReadonlySet<string>; routeHostnames: ReadonlySet<string> };
};

function resolveDefaultTargetPort(
  env: EnvironmentServiceViewRecord["service"]["env"] | undefined,
): number | null {
  const portValue = env?.["PORT"];
  if (portValue === undefined) return DEFAULT_SERVICE_PORT;
  if (portValue.kind !== "literal" || portValue.parts?.some((part) => part.kind === "ref")) return null;
  if (!/^\+?[0-9]+$/.test(portValue.value)) return null;
  const port = Number(portValue.value);
  return Number.isInteger(port) && port >= 1 && port <= 65535 ? port : null;
}

function serviceNodes(
  nodes: Array<{ nodeType: string; nodeId: string; config: unknown }>,
  serviceId: string,
): EnvironmentNodeProjection[] {
  return nodes
    .filter((node) => node.nodeType === "service" && node.nodeId === serviceId)
    .map((node) => ({
      node: { type: "service", id: serviceId },
      config: node.config ? parseServiceConfig(node.config) : null,
    }));
}

export function useServiceDrawerState(
  params: ServiceRouteParams,
): ServiceDrawerState | null {
  const editMetadata = useServiceMetadataEditor(params.organizationSlug);
  const collectionScope = useCollectionScope();
  const collection = useServicesCollection(params.organizationSlug);
  const serviceWriter = useServiceWriter(params.organizationSlug);
  const canvasPositions = useCanvasPositionsCollection(params.organizationSlug);
  const documents = getEnvironmentDocumentsCollection(params.organizationSlug, collectionScope);
  const nodeIntroductions = getEnvironmentNodeIntroductionsCollection(
    params.organizationSlug, collectionScope,
  );
  const environmentMatch = useMatch({
    from: ENVIRONMENT_ROUTE_FROM,
    shouldThrow: false,
  });
  const environmentId =
    environmentMatch?.loaderData?.environmentId ??
    collection.get(params.serviceId)?.environmentId ??
    null;
  const environmentChangeState = useEnvironmentChangeStateProjection({
    organizationSlug: params.organizationSlug,
    environmentId,
  });
  const { data: rawServices } = useLiveSuspenseQuery(
    { queryKey: ['drawer-services', collection.id, canvasPositions.id, documents.id, params.projectSlug, params.environmentSlug], query: (q) =>
      buildEnvironmentServicesViewQuery(q, params, {
        services: collection,
        canvasPositions,
        documents,
      }) },
  );
  const { data: serviceIntroductionRows } = useLiveSuspenseQuery({
    queryKey: ['service-introduction', nodeIntroductions.id, params.serviceId],
    query: (q) =>
      q
        .from({ introduction: nodeIntroductions })
        .where(({ introduction }) => eq(introduction.nodeType, "service"))
        .where(({ introduction }) => eq(introduction.nodeId, params.serviceId))
        .select(({ introduction }) => introduction),
  });

  const services = rawServices.map(normalizeEnvironmentServicesViewRecord);
  const serviceView = services.find(
    (item) => item.service.id === params.serviceId,
  );
  const service = serviceView?.service;
  const serviceIntroductionRow = serviceIntroductionRows[0];
  const serviceIntroduction = serviceIntroductionRow
    ? decodeStrict(environmentNodeIntroductionSchema, {
        organizationId: serviceIntroductionRow.organizationId,
        environmentId: serviceIntroductionRow.environmentId,
        nodeType: serviceIntroductionRow.nodeType,
        nodeId: serviceIntroductionRow.nodeId,
        nodeLineageId: serviceIntroductionRow.nodeLineageId,
        configVersion: serviceIntroductionRow.configVersion,
        config: serviceIntroductionRow.config,
        createdAt: serviceIntroductionRow.createdAt,
        updatedAt: serviceIntroductionRow.updatedAt,
      })
    : null;

  if (!service) {
    // Suspense only resumes after the org service collection is loaded, so an
    // absent id means the service is verifiably gone (e.g. deleted while open).
    // Close the drawer by
    // redirecting back to the environment overview instead of erroring.
    throw redirect({
      to: ENVIRONMENT_INDEX_ROUTE_TO,
      params: {
        organizationSlug: params.organizationSlug,
        projectSlug: params.projectSlug,
        environmentSlug: params.environmentSlug,
      },
      replace: true,
    });
  }

  const node = { type: "service" as const, id: params.serviceId };
  const change = environmentChangeState
    ? buildEnvironmentNodeChange({
        working: { node, config: projectServiceDeploymentConfig(service) },
        applied: serviceNodes(environmentChangeState.applied.nodes, params.serviceId),
        saved: environmentChangeState.saved
          ? serviceNodes(environmentChangeState.saved.nodes, params.serviceId)
          : null,
        submitted: environmentChangeState.deploymentEvidence
          ? serviceNodes(environmentChangeState.deploymentEvidence.nodes, params.serviceId)
          : null,
        introduction:
          serviceIntroduction?.nodeType === "service"
            ? { node, config: serviceIntroduction.config }
            : null,
      })
    : null;

  return {
    environmentSlug: params.environmentSlug,
    organizationSlug: params.organizationSlug,
    service,
    environmentNodes: services.map((item) => ({
      type: "service" as const,
      id: item.service.id,
      name: item.service.name,
    })),
    diff: getServiceDeploymentDiffState(change),
    collection: serviceWriter,
    editMetadata,
    managedPrefixesInUse: services
      .filter((item) => item.service.id !== service.id)
      .flatMap((item) => item.service.managedHostnames.map((m) => m.prefix)),
    defaultTargetPort: resolveDefaultTargetPort(service.env),
    appliedDomains: appliedDomains(environmentChangeState?.applied.nodes ?? [], params.serviceId),
  };
}

function appliedDomains(
  nodes: Array<{ nodeType: string; nodeId: string; config: unknown }>,
  serviceId: string,
): ServiceDrawerState["appliedDomains"] {
  const node = nodes.find((candidate) => candidate.nodeType === "service" && candidate.nodeId === serviceId);
  const config = node?.config ? parseServiceConfig(node.config) : null;
  return {
    managedPrefixes: new Set(config?.managedHostnames.map((managed) => managed.prefix)),
    routeHostnames: new Set(config?.routes.map((route) => route.hostname)),
  };
}
