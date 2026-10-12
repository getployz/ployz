import { useLiveQuery } from "@tanstack/react-db";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { type Requirement, serverSupports } from "#/modules/runtime/daemon-compat";
import { getRuntimeCollections } from "#/modules/runtime/runtime.collection";

export function useServerSupports(organizationSlug: string, machineId: string, requirement: Requirement) {
  const { data: machines = [] } = useLiveQuery(getRuntimeCollections(organizationSlug, useCollectionScope()).machines);
  const machine = machines.find((row) => row.id === machineId);
  return machine === undefined ? "unknown" : serverSupports(machine, requirement);
}
