import { getEnvironmentDocumentsCollection } from "#/modules/environment-design/environment-document.collection";
import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { redirect, useMatch } from "@tanstack/react-router";
import {
  ENVIRONMENT_INDEX_ROUTE_TO,
  ENVIRONMENT_ROUTE_FROM,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";
import type { EnvironmentChangeStateNodeProjection } from "#/modules/deployments/deployment-contract";
import {
  getServiceDeploymentDiffState,
  type ServiceDeploymentDiffState,
} from "#/modules/services/service-deployment-diff/state";
import { resolveEnvironmentWorkingComparison } from "#/modules/environment-design/environment-change-set";
import {
  buildEnvironmentServicesViewQuery,
  type ServiceWriter,
  type EnvironmentServiceViewRecord,
  normalizeEnvironmentServicesViewRecord,
  useCanvasPositionsCollection,
  useEnvironmentResourcesCollection,
  useServicesCollection,
  useServiceWriter,
} from "#/modules/services/services.collection";
import { useEnvironmentChangeStateProjection } from "#/modules/deployments/use-environment-state-projection";
import type { EnvironmentNodeNameIdentity } from "#/modules/environment-design/environment-node-names";
import { automaticBoundHostnames } from "#/modules/runtime/runtime";
import { getEnvironmentNodeIntroductionsCollection } from "#/electric/collections";
import { environmentNodeIntroductionSchema } from "#/modules/environment-design/environment-node-introductions";
import { decodeStrict } from "#/modules/environment-design/schema";
import { parseLiveQueryRow } from "#/lib/tanstack-db";
import { variableGroupResourceRecordSchema } from "#/modules/environment-design/resources";
import {
  useRuntimePublicUrl,
  useRuntimeService,
} from "#/providers/runtime-provider";

export type ServiceRouteParams = {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
  serviceId: string;
};

export type ServiceDrawerState = {
  organizationSlug: string;
  service: EnvironmentServiceViewRecord["service"];
  environmentNodes: EnvironmentNodeNameIdentity[];
  diff: ServiceDeploymentDiffState;
  collection: ServiceWriter;
  /** Managed-domain prefixes already claimed by other services in this
   * environment, for client-side uniqueness hints (server validates org-wide). */
  managedPrefixesInUse: string[];
  /** The port a new public domain targets by default: the service's PORT env
   * var when set, else 8080. */
  defaultTargetPort: number;
};

function resolveDefaultTargetPort(
  env: EnvironmentServiceViewRecord["service"]["env"] | undefined,
): number {
  const portValue = env?.["PORT"];
  const raw = portValue?.kind === "literal" ? portValue.value : undefined;
  const port = Number(raw);
  return Number.isInteger(port) && port >= 1 && port <= 65535 ? port : 8080;
}

function serviceConfig(
  nodes: EnvironmentChangeStateNodeProjection[],
  serviceId: string,
) {
  const node = nodes.find(
    (candidate) =>
      candidate.nodeType === "service" && candidate.nodeId === serviceId,
  );
  return node?.nodeType === "service" ? node.config : null;
}

export function useServiceDrawerState(
  params: ServiceRouteParams,
): ServiceDrawerState | null {
  const collection = useServicesCollection(params.organizationSlug);
  const serviceWriter = useServiceWriter(params.organizationSlug);
  const environmentResourcesCollection = useEnvironmentResourcesCollection(
    params.organizationSlug,
  );
  const canvasPositions = useCanvasPositionsCollection(params.organizationSlug);
  const documents = getEnvironmentDocumentsCollection(params.organizationSlug);
  const nodeIntroductions = getEnvironmentNodeIntroductionsCollection(
    params.organizationSlug,
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
    (q) =>
      buildEnvironmentServicesViewQuery(q, params, {
        services: collection,
        canvasPositions,
        documents,
      }),
    [
      canvasPositions,
      collection,
      params.environmentSlug,
      params.projectSlug,
      documents,
    ],
  );
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
  const { data: serviceIntroductionRows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ introduction: nodeIntroductions })
        .where(({ introduction }) => eq(introduction.nodeType, "service"))
        .where(({ introduction }) => eq(introduction.nodeId, params.serviceId))
        .select(({ introduction }) => introduction),
  });
  // Runtime state for cluster-domain drift on the managed hostname. Keyed by the
  // runtime service id (== privateDns); read from the collection so the hooks stay
  // unconditional even before the live query resolves.
  const privateDns = collection.get(params.serviceId)?.privateDns ?? "";
  const { runtime } = useRuntimeService(params.environmentSlug, privateDns);
  const publicUrl = useRuntimePublicUrl();
  const environmentResources = environmentResourceRows.map((resource) =>
    parseLiveQueryRow(variableGroupResourceRecordSchema, resource),
  );

  const services = rawServices.map(normalizeEnvironmentServicesViewRecord);
  const serviceView = services.find(
    (item) => item.service.id === params.serviceId,
  );
  const service = serviceView?.service;
  const serviceIntroductionRow = serviceIntroductionRows[0];
  const serviceIntroduction = serviceIntroductionRow
    ? decodeStrict(environmentNodeIntroductionSchema, {
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

  const saved = environmentChangeState?.saved
    ? serviceConfig(environmentChangeState.saved.nodes, params.serviceId)
    : null;
  const applied = environmentChangeState
    ? serviceConfig(environmentChangeState.applied.nodes, params.serviceId)
    : null;
  const introduction =
    serviceIntroduction?.nodeType === "service"
      ? serviceIntroduction.config
      : null;

  return {
    organizationSlug: params.organizationSlug,
    service,
    environmentNodes: [
      ...services.map((item) => ({
        type: "service" as const,
        id: item.service.id,
        name: item.service.name,
      })),
      ...environmentResources.map((item) => ({
        type: "variable_group" as const,
        id: item.resource.id,
        name: item.resource.name,
      })),
    ],
    diff: getServiceDeploymentDiffState({
      service,
      comparison: resolveEnvironmentWorkingComparison({
        saved,
        applied,
        introduction,
      }),
      managedHostnameRuntime: {
        autoDomain: publicUrl.autoDomain,
        boundHostnames: automaticBoundHostnames(runtime),
      },
    }),
    collection: serviceWriter,
    managedPrefixesInUse: services
      .filter(
        (item) =>
          item.service.id !== service.id &&
          item.service.managedHostname != null,
      )
      .map((item) => item.service.managedHostname?.prefix ?? ""),
    defaultTargetPort: resolveDefaultTargetPort(service.env),
  };
}
