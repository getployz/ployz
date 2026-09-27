import { useCollectionScope } from "#/collections/use-collection-scope";
import { useLiveQuery } from "@tanstack/react-db";
import {
  getRuntimeCollections,
  projectRuntimeMachineRecord,
} from "#/modules/runtime/runtime.collection";

export function useRuntimeLens(organizationSlug: string) {
  const collections = getRuntimeCollections(organizationSlug, useCollectionScope());
  const { data: machines = [], isLoading: machinesLoading } = useLiveQuery(collections.machines);
  const { data: statusRows = [], isLoading: statusLoading } = useLiveQuery(collections.status);
  const status = statusRows[0]?.status ?? "connecting";
  const error = statusRows[0]?.error ?? null;
  const incompleteIds = statusRows[0]?.incompleteIds;
  const isLoading = machinesLoading || statusLoading;

  return {
    machines: machines.map(projectRuntimeMachineRecord),
    status,
    error,
    /** The observation left Machines or containers unread, so what it shows may be missing things. */
    incomplete: (incompleteIds?.machines.length ?? 0) + (incompleteIds?.containers.length ?? 0) > 0,
    isLoading,
  };
}
