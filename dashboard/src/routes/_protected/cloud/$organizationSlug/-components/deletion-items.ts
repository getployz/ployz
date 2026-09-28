import { useLiveQuery } from "@tanstack/react-db";
import { useOrgStoreStatus } from "#/collections/org-store";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { DeletionItem } from "#/components/deletion-dialog";
import { withoutVirtualProps } from "#/lib/tanstack-db";
import { useLineageNames } from "#/modules/branches/use-lineage-names";
import type { EnvironmentChangeStateProjection } from "#/modules/deployments/deployment-contract";
import { useEnvironmentChangeStatesIfReady } from "#/modules/deployments/environment-change-state.queries";
import type { ServiceWithContextRecord } from "#/modules/environment-design/service-schemas";
import { findEnvironment, useWorkspace } from "#/modules/environment-design/workspace.queries";
import { getServicesCollection, getVolumeResourcesCollection } from "#/modules/services/services.collection";
import { getServiceIcon } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/service-node-helpers";

type Where = { readonly projectSlug: string; readonly environmentSlug: string };

/**
 * The services and volumes a deletion takes, drawn as the canvas draws them: one Environment's, a project's, or the
 * whole organization's. That is what runs (Applied State) as well as what's authored (Working State): a staged delete
 * leaves Working State at once, but the service runs until the next deploy. Past one Environment, each says where it is.
 */
export function useDeletionNodes(organizationSlug: string, within: Partial<Where> = {}): DeletionItem[] {
  const scope = useCollectionScope();
  const ready = useOrgStoreStatus(organizationSlug).data;
  const { projects, environments } = useWorkspace(organizationSlug);
  const applied = useEnvironmentChangeStatesIfReady(organizationSlug) ?? [];
  const nameOf = useLineageNames(organizationSlug);
  const services = ready ? getServicesCollection(organizationSlug, scope) : null;
  const volumes = ready ? getVolumeResourcesCollection(organizationSlug, scope) : null;
  const serviceRows = useLiveQuery({
    queryKey: ["deletion-services", services?.id ?? null],
    query: (q) => services
      ? q.from({ service: services }).select(({ service }) => ({
        id: service.id, name: service.name, source: service.source,
        projectSlug: service.projectSlug, environmentSlug: service.environmentSlug,
      }))
      : undefined,
  });
  const volumeRows = useLiveQuery({
    queryKey: ["deletion-volumes", volumes?.id ?? null],
    query: (q) => volumes
      ? q.from({ volume: volumes }).select(({ volume }) => ({
        id: volume.resource.id, name: volume.resource.name, projectSlug: volume.projectSlug, environmentSlug: volume.environmentSlug,
      }))
      : undefined,
  });
  return deletionNodes({
    within, projects, environments, applied, nameOf,
    services: (serviceRows.data ?? []).map(withoutVirtualProps),
    volumes: (volumeRows.data ?? []).map(withoutVirtualProps),
  });
}

type Located = Where & { readonly id: string; readonly name: string };

/** What runs in each Environment in scope, then what's authored there, by node id: a name edited since the deploy wins. */
export function deletionNodes({ within, projects, environments, applied, nameOf, services, volumes }: {
  within: Partial<Where>;
  projects: readonly { id: string; slug: string; name: string }[];
  environments: readonly { id: string; projectId: string; namespace: string; name: string }[];
  applied: readonly Pick<EnvironmentChangeStateProjection, "environmentId" | "applied">[];
  nameOf: (lineage: string, environmentId: string) => string;
  services: readonly (Located & Pick<ServiceWithContextRecord, "source">)[];
  volumes: readonly Located[];
}): DeletionItem[] {
  const projectOf = (environment: { projectId: string }) => projects.find((project) => project.id === environment.projectId);
  const inScope = environments.filter((environment) =>
    (within.projectSlug === undefined || projectOf(environment)?.slug === within.projectSlug)
    && (within.environmentSlug === undefined || environment.namespace === within.environmentSlug));
  // Where a node is below the scope: "production" in a project, "shop/production" in the organization.
  const where = (environment: { name: string; projectId: string }) => within.environmentSlug !== undefined
    ? undefined
    : within.projectSlug !== undefined ? environment.name : `${projectOf(environment)?.name ?? ""}/${environment.name}`;

  const nodes = new Map<string, DeletionItem>();
  for (const environment of inScope) {
    for (const node of applied.find((state) => state.environmentId === environment.id)?.applied.nodes ?? []) {
      nodes.set(node.nodeId, node.nodeType === "service"
        ? { kind: "service", name: nameOf(node.nodeLineageId, environment.id), icon: getServiceIcon(node.config), detail: where(environment) }
        : { kind: "volume", name: node.config.name, detail: where(environment) });
    }
  }
  for (const row of services) {
    const environment = findEnvironment(projects, inScope, row);
    if (environment) nodes.set(row.id, { kind: "service", name: row.name, icon: getServiceIcon(row), detail: where(environment) });
  }
  for (const row of volumes) {
    const environment = findEnvironment(projects, inScope, row);
    if (environment) nodes.set(row.id, { kind: "volume", name: row.name, detail: where(environment) });
  }
  return [...nodes.values()];
}

/** Where an Environment is, as its breadcrumbs name it, and what the user types to delete from it: "shop/staging". */
export function useEnvironmentPlace(organizationSlug: string, environmentId: string) {
  const { projects, environments } = useWorkspace(organizationSlug);
  const environment = environments.find((row) => row.id === environmentId);
  const project = projects.find((row) => row.id === environment?.projectId);
  return `${project?.name ?? ""}/${environment?.name ?? ""}`;
}
