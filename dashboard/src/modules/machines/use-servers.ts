import { useLiveQuery, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getRuntimeCollections, projectRuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { useServicesCollection } from "#/modules/services/services.collection";
import { servicesOnServers } from "./server-services";
import { sortServers, useServerStatuses } from "./server-status";

/**
 * The Organization's Servers as the Servers pages show them: a status each, the Services running on it, problems first.
 * Reads the Org Store synchronously, so only pages below the Org Store gate may call it.
 */
export function useServers(organizationSlug: string) {
  const runtime = useRuntimeLens(organizationSlug);
  const statusOf = useServerStatuses(runtime.machines);
  const { data: runtimeServices = [] } = useLiveQuery(getRuntimeCollections(organizationSlug, useCollectionScope()).services);
  const { data: cloudServices } = useLiveSuspenseQuery(useServicesCollection(organizationSlug));
  const services = servicesOnServers(runtimeServices.map(projectRuntimeServiceRecord), cloudServices);
  const servers = sortServers(runtime.machines.map((machine) => ({
    machine,
    name: machine.name,
    status: statusOf(machine),
    services: services.filter((service) => service.machineIds.has(machine.id)),
  })));
  return {
    lens: runtime.status,
    /** The last observation is shown while the Runtime Watch is down. */
    stale: runtime.status === "unavailable",
    servers,
  };
}

export type Server = ReturnType<typeof useServers>["servers"][number];
