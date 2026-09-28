import { eq, useLiveQuery } from "@tanstack/react-db";
import { useOrgStoreStatus } from "#/collections/org-store";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { DeletionItem } from "#/components/deletion-dialog";
import { withoutVirtualProps } from "#/lib/tanstack-db";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { getServicesCollection, getVolumeResourcesCollection, type EnvironmentParams } from "#/modules/services/services.collection";
import { getServiceIcon } from "./canvas/service-node-helpers";

/** An Environment's services and volumes as the canvas draws them, for what its deletion takes. */
export function useEnvironmentDeletionItems({ organizationSlug, projectSlug, environmentSlug }: EnvironmentParams): DeletionItem[] {
  const scope = useCollectionScope();
  const ready = useOrgStoreStatus(organizationSlug).data;
  const services = ready ? getServicesCollection(organizationSlug, scope) : null;
  const volumes = ready ? getVolumeResourcesCollection(organizationSlug, scope) : null;
  const serviceRows = useLiveQuery({
    queryKey: ["deletion-services", services?.id ?? null, projectSlug, environmentSlug],
    query: (q) => services
      ? q.from({ service: services })
        .where(({ service }) => eq(service.projectSlug, projectSlug))
        .where(({ service }) => eq(service.environmentSlug, environmentSlug))
        .select(({ service }) => ({ name: service.name, source: service.source }))
      : undefined,
  });
  const volumeRows = useLiveQuery({
    queryKey: ["deletion-volumes", volumes?.id ?? null, projectSlug, environmentSlug],
    query: (q) => volumes
      ? q.from({ volume: volumes })
        .where(({ volume }) => eq(volume.projectSlug, projectSlug))
        .where(({ volume }) => eq(volume.environmentSlug, environmentSlug))
        .select(({ volume }) => ({ name: volume.resource.name }))
      : undefined,
  });
  return [
    ...(serviceRows.data ?? []).map(withoutVirtualProps)
      .map((row): DeletionItem => ({ kind: "service", name: row.name, icon: getServiceIcon(row) })),
    ...(volumeRows.data ?? []).map(withoutVirtualProps).map((row): DeletionItem => ({ kind: "volume", name: row.name })),
  ];
}

/** Where an Environment is, as its breadcrumbs name it, and what the user types to delete from it: "shop/staging". */
export function useEnvironmentPlace(organizationSlug: string, environmentId: string) {
  const { projects, environments } = useWorkspace(organizationSlug);
  const environment = environments.find((row) => row.id === environmentId);
  const project = projects.find((row) => row.id === environment?.projectId);
  return `${project?.name ?? ""}/${environment?.name ?? ""}`;
}
