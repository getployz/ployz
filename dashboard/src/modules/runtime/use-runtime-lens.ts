import { useLiveQuery } from "@tanstack/react-db";
import {
  EMPTY_RUNTIME_INCOMPLETE_IDS,
  getRuntimeCollections,
  projectRuntimeMachineRecord,
} from "#/modules/runtime/runtime.collection";

export function useRuntimeLens(organizationSlug: string) {
  const collections = getRuntimeCollections({ organizationSlug });
  const { data: machines = [], isLoading: machinesLoading } = useLiveQuery({
    query: (q) =>
      q.from({ machine: collections.machines }).select(({ machine }) => machine)
  });
  const { data: statusRows = [], isLoading: statusLoading } = useLiveQuery({
    query: (q) =>
      q.from({ status: collections.status }).select(({ status }) => status)
  });
  const status = statusRows[0]?.status ?? "connecting";
  const error = statusRows[0]?.error ?? null;
  const incompleteIds =
    statusRows[0]?.incompleteIds ?? EMPTY_RUNTIME_INCOMPLETE_IDS;
  const isLoading = machinesLoading || statusLoading;

  return {
    machines: machines.map(projectRuntimeMachineRecord),
    status,
    error,
    incompleteIds,
    isLoading,
  };
}
