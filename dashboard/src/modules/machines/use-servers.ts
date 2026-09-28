import { useLiveQuery, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getRuntimeCollections, projectRuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { useServicesCollection } from "#/modules/services/services.collection";
import { servicesOnServers } from "./server-services";
import { serverListState, serverStatus, sortServers } from "./server-status";

/** The Organization's Servers from the Runtime alone, problems first. Safe in chrome above the Org Store gate. */
export function useServerList(organizationSlug: string) {
  const runtime = useRuntimeLens(organizationSlug);
  const servers = sortServers(runtime.machines.map((machine) => ({ machine, name: machine.name, status: serverStatus(machine) })));
  return { state: serverListState(runtime.status, servers.length, runtime.incomplete), servers };
}

export type ServerListItem = ReturnType<typeof useServerList>["servers"][number];

/** Each Server with the Services running on it. Reads the Org Store, so only pages below its gate may call it. */
export function useServers(organizationSlug: string) {
  const list = useServerList(organizationSlug);
  const { data: runtimeServices = [] } = useLiveQuery(getRuntimeCollections(organizationSlug, useCollectionScope()).services);
  const { data: cloudServices } = useLiveSuspenseQuery(useServicesCollection(organizationSlug));
  const services = servicesOnServers(runtimeServices.map(projectRuntimeServiceRecord), cloudServices);
  return {
    state: list.state,
    servers: list.servers.map((server) => ({
      ...server,
      services: services.filter((service) => service.machineIds.has(server.machine.id)),
    })),
  };
}

export type Server = ReturnType<typeof useServers>["servers"][number];
