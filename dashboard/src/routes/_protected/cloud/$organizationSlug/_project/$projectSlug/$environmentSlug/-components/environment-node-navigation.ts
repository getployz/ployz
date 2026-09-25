import { useOrgStoreStatus } from "#/collections/org-store";
import { eq, useLiveQuery } from "@tanstack/react-db";
import { withoutVirtualProps } from "#/lib/tanstack-db";
import { linkOptions } from "@tanstack/react-router";
import { useCollectionScope } from "#/collections/use-collection-scope";
import {
  getServicesCollection,
  getVolumeResourcesCollection,
  type EnvironmentParams,
} from "#/modules/services/services.collection";
import type { DeploymentServicePage, ServicePage } from "../services/$serviceId/-components/service-pages";
import {
  ENVIRONMENT_SERVICE_ROUTE_TO,
  ENVIRONMENT_RESOURCE_ROUTE_TO,
} from "./environment-route-paths";

export type NavigationNode = {
  id: string;
  name: string;
  type: "service" | "volume";
};

export function nodeDestination(
  params: EnvironmentParams,
  node: NavigationNode,
  page?: ServicePage | DeploymentServicePage,
) {
  const { organizationSlug, projectSlug, environmentSlug } = params;
  return node.type === "service"
    ? linkOptions({
        to: ENVIRONMENT_SERVICE_ROUTE_TO,
        params: {
          organizationSlug,
          projectSlug,
          environmentSlug,
          serviceId: node.id,
        },
        search: { tab: page },
      })
    : linkOptions({
        to: ENVIRONMENT_RESOURCE_ROUTE_TO,
        params: {
          organizationSlug,
          projectSlug,
          environmentSlug,
          resourceId: node.id,
        },
        search: {},
      });
}

export function useEnvironmentNavigationNodes(params: EnvironmentParams) {
  const scope = useCollectionScope();
  const { organizationSlug, projectSlug, environmentSlug } = params;
  const ready = useOrgStoreStatus(organizationSlug);
  const services = ready.data
    ? getServicesCollection(organizationSlug, scope)
    : null;
  const volumes = ready.data
    ? getVolumeResourcesCollection(organizationSlug, scope)
    : null;
  const serviceRows = useLiveQuery(
    { queryKey: ['navigation-services', services?.id ?? null, projectSlug, environmentSlug], query: (q) =>
      services
        ? q
            .from({ service: services })
            .where(({ service }) => eq(service.projectSlug, projectSlug))
            .where(({ service }) =>
              eq(service.environmentSlug, environmentSlug),
            )
            .select(({ service }) => ({ id: service.id, name: service.name }))
        : undefined },
  );
  const volumeRows = useLiveQuery(
    { queryKey: ['navigation-volumes', volumes?.id ?? null, projectSlug, environmentSlug], query: (q) =>
      volumes
        ? q
            .from({ volume: volumes })
            .where(({ volume }) => eq(volume.projectSlug, projectSlug))
            .where(({ volume }) => eq(volume.environmentSlug, environmentSlug))
            .select(({ volume }) => ({
              id: volume.resource.id,
              name: volume.resource.name,
            }))
        : undefined },
  );

  const nodes: NavigationNode[] = [
    ...(serviceRows.data ?? []).map(withoutVirtualProps).map(({ id, name }) => ({
      id,
      name,
      type: "service" as const,
    })),
    ...(volumeRows.data ?? []).map(withoutVirtualProps).map(({ id, name }) => ({
      id,
      name,
      type: "volume" as const,
    })),
  ];
  return {
    nodes,
    isLoading:
      ready.isPending ||
      serviceRows.isLoading ||
      volumeRows.isLoading,
    isError:
      ready.isError ||
      serviceRows.isError ||
      volumeRows.isError,
    retry: () => void ready.refetch(),
  };
}
