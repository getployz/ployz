import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { parseLiveQueryRow } from "#/lib/tanstack-db";
import {
  useEnvironmentResourcesCollection,
  useServicesCollection,
} from "#/modules/services/services.collection";
import {
  variableGroupResourceRecordSchema,
  type VariableGroupResourceRecord,
} from "#/modules/environment-design/resources";
import type { EnvironmentNodeNameIdentity } from "#/modules/environment-design/environment-node-names";

export type VariableGroupResourceRouteParams = {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
  resourceId: string;
};

export type VariableGroupDrawerState = {
  organizationSlug: string;
  resource: VariableGroupResourceRecord;
  environmentNodes: EnvironmentNodeNameIdentity[];
};

export function useVariableGroupDrawerState(
  params: VariableGroupResourceRouteParams,
): VariableGroupDrawerState | null {
  const environmentResources = useEnvironmentResourcesCollection(
    params.organizationSlug,
  );
  const servicesCollection = useServicesCollection(params.organizationSlug);
  const { data: resourceRows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ resource: environmentResources })
        .where(({ resource }) => eq(resource.projectSlug, params.projectSlug))
        .where(({ resource }) =>
          eq(resource.environmentSlug, params.environmentSlug),
        )
        .select(({ resource }) => resource),
  });
  const resources = resourceRows.map((resource) =>
    parseLiveQueryRow(variableGroupResourceRecordSchema, resource),
  );
  const { data: services } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ service: servicesCollection })
        .where(({ service }) => eq(service.projectSlug, params.projectSlug))
        .where(({ service }) =>
          eq(service.environmentSlug, params.environmentSlug),
        )
        .select(({ service }) => service),
  });
  const resource =
    resources.find((item) => item.resource.id === params.resourceId) ?? null;

  if (!resource) {
    return null;
  }

  return {
    organizationSlug: params.organizationSlug,
    resource,
    environmentNodes: [
      ...services.map((service) => ({
        type: "service" as const,
        id: service.id,
        name: service.name,
      })),
      ...resources.map((item) => ({
        type: "variable_group" as const,
        id: item.resource.id,
        name: item.resource.name,
      })),
    ],
  };
}
