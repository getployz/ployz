import { useLiveQuery } from "@tanstack/react-db";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { AWAITS_KEY_ERROR, getRuntimeCollections, OLDER_SERVERS_ERROR, PREDATES_SEALING_ERROR, projectRuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { servicesOnServers } from "./server-services";
import { serverListState, serverStatus, sortServers } from "./server-status";

/** The Organization's Servers from the Runtime alone, problems first. Safe in chrome above the Org Store gate. */
export function useServerList(organizationSlug: string) {
  const runtime = useRuntimeLens(organizationSlug);
  const servers = sortServers(runtime.machines.map((machine) => ({ machine, name: machine.name, status: serverStatus(machine) })));
  return {
    state: serverListState(runtime.status, servers.length, runtime.incomplete),
    servers,
    /** Cloud can't use a Server because it runs an older Ployz; only its upgrade over SSH lets it. */
    older: runtime.status !== "unreachable" ? null : olderServers(runtime.error),
    /** Cloud can't use a Server that answers until a Server holding the cluster's key is back to share it. */
    awaitingKey: runtime.status === "unreachable" && runtime.error === AWAITS_KEY_ERROR,
  };
}

/** Which older Ployz keeps Cloud from a Server: one it can't reach through the relay, or one that predates the Sealing Key. */
export type OlderServers = "relay" | "sealing";

function olderServers(error: string | null): OlderServers | null {
  if (error === OLDER_SERVERS_ERROR) return "relay";
  if (error === PREDATES_SEALING_ERROR) return "sealing";
  return null;
}

export type ServerListItem = ReturnType<typeof useServerList>["servers"][number];

/** Each Server with the Services running on it. */
export function useServers(organizationSlug: string) {
  const list = useServerList(organizationSlug);
  const { data: runtimeServices = [] } = useLiveQuery(getRuntimeCollections(organizationSlug, useCollectionScope()).services);
  const services = servicesOnServers(runtimeServices.map(projectRuntimeServiceRecord));
  return {
    state: list.state,
    older: list.older,
    awaitingKey: list.awaitingKey,
    servers: list.servers.map((server) => ({
      ...server,
      services: services.filter((service) => service.machineIds.has(server.machine.id)),
    })),
  };
}

export type Server = ReturnType<typeof useServers>["servers"][number];
