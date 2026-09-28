import { useLiveQuery } from "@tanstack/react-db";
import { useOrgStoreStatus } from "#/collections/org-store";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { DeletionItem } from "#/components/deletion-dialog";
import { withoutVirtualProps } from "#/lib/tanstack-db";
import { findEnvironment, useWorkspace } from "#/modules/environment-design/workspace.queries";
import { getServicesCollection, getVolumeResourcesCollection } from "#/modules/services/services.collection";
import { getServiceIcon } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/service-node-helpers";

type Where = { readonly projectSlug: string; readonly environmentSlug: string };

/**
 * The services and volumes a deletion takes, drawn as the canvas draws them: one Environment's, a project's, or the
 * whole organization's. Past one Environment, each says where it is.
 */
export function useDeletionNodes(organizationSlug: string, within: Partial<Where> = {}): DeletionItem[] {
  const scope = useCollectionScope();
  const ready = useOrgStoreStatus(organizationSlug).data;
  const { projects, environments } = useWorkspace(organizationSlug);
  const services = ready ? getServicesCollection(organizationSlug, scope) : null;
  const volumes = ready ? getVolumeResourcesCollection(organizationSlug, scope) : null;
  const serviceRows = useLiveQuery({
    queryKey: ["deletion-services", services?.id ?? null],
    query: (q) => services
      ? q.from({ service: services }).select(({ service }) => ({
        name: service.name, source: service.source, projectSlug: service.projectSlug, environmentSlug: service.environmentSlug,
      }))
      : undefined,
  });
  const volumeRows = useLiveQuery({
    queryKey: ["deletion-volumes", volumes?.id ?? null],
    query: (q) => volumes
      ? q.from({ volume: volumes }).select(({ volume }) => ({
        name: volume.resource.name, projectSlug: volume.projectSlug, environmentSlug: volume.environmentSlug,
      }))
      : undefined,
  });
  const inside = (row: Where) => (within.projectSlug === undefined || row.projectSlug === within.projectSlug)
    && (within.environmentSlug === undefined || row.environmentSlug === within.environmentSlug);
  // Where a node is below the scope: "production" in a project, "shop/production" in the organization.
  const where = (row: Where) => {
    if (within.environmentSlug !== undefined) return undefined;
    const environment = findEnvironment(projects, environments, row)?.name ?? row.environmentSlug;
    return within.projectSlug !== undefined
      ? environment
      : `${projects.find((project) => project.slug === row.projectSlug)?.name ?? row.projectSlug}/${environment}`;
  };
  return [
    ...(serviceRows.data ?? []).map(withoutVirtualProps).filter(inside)
      .map((row): DeletionItem => ({ kind: "service", name: row.name, icon: getServiceIcon(row), detail: where(row) })),
    ...(volumeRows.data ?? []).map(withoutVirtualProps).filter(inside)
      .map((row): DeletionItem => ({ kind: "volume", name: row.name, detail: where(row) })),
  ];
}

/** Where an Environment is, as its breadcrumbs name it, and what the user types to delete from it: "shop/staging". */
export function useEnvironmentPlace(organizationSlug: string, environmentId: string) {
  const { projects, environments } = useWorkspace(organizationSlug);
  const environment = environments.find((row) => row.id === environmentId);
  const project = projects.find((row) => row.id === environment?.projectId);
  return `${project?.name ?? ""}/${environment?.name ?? ""}`;
}
